//! The review list: one flat, virtualized `PortalList` with a template per row
//! kind (prose, file-card chrome, code rows, comments, the inline composer).
//! Every dock tab owns one instance, pinned to a stream by its `kind`. The code
//! rows compose the `Gutter` + `DiffLine` pair; a sticky overlay mirrors the
//! card header that has scrolled past the top so its path and tick box stay in
//! reach. Both headers emit the same actions, addressed by file path.

mod editing;

use std::{cell::Cell, collections::HashSet, sync::Arc};

use concats_diff::{Blob, LineKind, Row};
use concats_review::store::{self, LineKey, Seen};

use super::{
    DiffLine, Gutter, collapsed_run::CollapsedRun, drop_shadow::DropShadow, gutter::RowMarks,
};
#[allow(
    clippy::wildcard_imports,
    reason = "Makepad macros and derives expand against the widget prelude in this scope."
)]
use crate::{
    FrameData, FrameTheme,
    find::Find,
    makepad_widgets::{event::TouchState, *},
    review_doc::{Composing, FileView, ReviewDoc, Stream, card_shut, compose_title, header_title},
    service::{HighlightCmd, ReviewState, highlight},
};

thread_local! {
    /// The draw thread's own highlighter, so a row gets its colours as it is
    /// drawn.
    ///
    /// A second one next to the worker's, and cheap: a grammar table and a
    /// cache of parsed trees, both keyed by content. We need the answer during
    /// the draw. Asking the worker paints the frame before the reply comes
    /// back, and that is the flash of plain text. One line off a parsed tree
    /// costs microseconds, so the draw just asks.
    static DRAW_HIGHLIGHTER: std::cell::RefCell<concats_highlight::Highlighter> =
        std::cell::RefCell::new(concats_highlight::Highlighter::new());
}

/// This row's colours, computed now.
///
/// For a blob under the budget this is the only source, not a fallback until
/// the worker's result lands. With two sources the colours of a frame depended
/// on whether the worker had finished. One source, no race, no flash.
///
/// It is the only source for a buffer with unsaved edits too, and there it is
/// the only correct one: the worker keys its trees by oid, typing does not
/// change the oid, so its answer would colour the text from before the edit.
fn draws_own_spans(blob: &Blob) -> bool {
    /// A file up to this size is parsed inline the first time it is drawn. A
    /// cold parse takes about 39 ms at this size and grows with it, so past
    /// some point the dropped frames cost more than a moment of uncoloured
    /// text. It is paid once per blob, which is why the bar is this generous.
    ///
    /// It is also the only bar: an edit reparses from the retained tree, about
    /// 4 ms at this size and well inside a frame, so a file we can parse is a
    /// file we can type in.
    const PARSE_UNDER: usize = 512 * 1024;

    blob.text.len() <= PARSE_UNDER
}

/// This row's colours, when the draw is the one colouring it.
fn row_spans(blob: &Blob, line: usize) -> Option<Vec<concats_syntax::Span>> {
    if draws_own_spans(blob) {
        return Some(DRAW_HIGHLIGHTER.with_borrow_mut(|hl| {
            hl.spans_for_line(
                concats_highlight::Buffer {
                    oid: blob.oid,
                    rev: blob.edit_rev,
                    editable: blob.editable(),
                    ext: &blob.ext,
                    text: &blob.text,
                    starts: &blob.line_starts,
                },
                line,
            )
        }));
    }
    None
}

/// The air between the end of a selection and the chip offering to comment on
/// it.
const CHIP_GAP: f64 = 8.0;
/// What the chip measures, near enough to know whether it fits above the
/// selection. It is one line of 8.25pt text in 4pt of padding.
const CHIP_HEIGHT: f64 = 24.0;
const STICKY_HEIGHT: f64 = 32.0;
const FILE_HEADER_TOP_PADDING: f64 = 16.0;
/// The pinned header rests this far below the top of the scroll area. The gap
/// is filled by the tab strip's shadow, not by bare page — see the design's
/// `02-header-pinned`, where the strip's falloff is cut short by the header.
const STICKY_TOP_GAP: f64 = 8.0;
/// Height of the pinned header's shadow, above it and below it alike.
const STICKY_SHADOW: f64 = 12.0;
/// The `CardEnd` row draws the card's bottom edge in its first 9pt (8pt of card
/// fill plus the 1pt border); the rest of the row is the gap to the next card.
/// The pinned header has to clear that edge, not the whole row, or it slides on
/// past the card it belongs to.
const CARD_END_EDGE: f64 = 9.0;
/// Scroll distance over which the pinned header's shadow ramps in. Toggling it
/// on a threshold put a 12pt margin change and a full-strength shadow on the
/// same pixel of scroll, so jitter around that point flashed.
const STICKY_FADE: f64 = 6.0;

/// Where one list entry was laid out this pass, from the list's height tree.
struct Band {
    entry: usize,
    top: f64,
    height: f64,
}

/// The window band one drawn code row occupies. Rows are not all the same
/// height — a long line wraps — so a pointer position names a row by its band.
struct DrawnRow {
    row: usize,
    top: f64,
    bottom: f64,
}

/// The drawn row under the viewport's top edge: the lowest entry whose band
/// still crosses y = 0, over the bands measured this pass. Anchoring on the
/// list's own first entry jitters by a row instead — zero-height rows (hunk
/// bars, skipped runs) and `PortalList`'s `first_id` bookkeeping both move it
/// — and a row of jitter at a card boundary makes the pinned header blink on
/// and off mid-scroll. A zero-height band never crosses the edge, so it cannot
/// win the pick.
fn anchor_entry(bands: &[Band]) -> Option<usize> {
    bands
        .iter()
        .filter(|band| band.top + band.height > 0.0)
        .map(|band| band.entry)
        .min()
}

/// The pinned header's offset and shadow strength — `(push, lift)` — from
/// where the owning card's header and end rows sit right now (`None`: not
/// drawn this pass, which the clamps treat as off-screen above and below
/// respectively). Pure, so the sticky design is testable without a widget:
/// ride the real header while it is on screen, rest at [`STICKY_TOP_GAP`],
/// slide out with the card's bottom edge, and ramp the shadow continuously.
// NOTE: (push, lift), as the doc names them: the tested pure half of place_sticky.
// coland-ignore: @coland/same_typed_tuple_return
fn sticky_offsets(header_top: Option<f64>, end_top: Option<f64>) -> (f64, f64) {
    // Where the real header sits right now, unclamped.
    let natural = header_top.map(|top| top + FILE_HEADER_TOP_PADDING);
    // Riding up: sit on the real header until it reaches the rest.
    let arrive = natural.unwrap_or(STICKY_TOP_GAP).max(STICKY_TOP_GAP);
    // Leaving: once the card's bottom edge reaches the top, the copy slides out
    // with it instead of covering the next card — the handoff CSS `position:
    // sticky` gives you for free.
    let leave = end_top.map_or(STICKY_TOP_GAP, |top| {
        (top + CARD_END_EDGE - STICKY_HEIGHT).clamp(-STICKY_HEIGHT, STICKY_TOP_GAP)
    });
    let push = if arrive > STICKY_TOP_GAP {
        arrive
    } else {
        leave
    };
    // How far the header floats above its resting place, 0..1 — the shadow's
    // opacity. It is continuous, so there is no scroll position where the
    // shadow suddenly appears.
    let lift = natural.map_or(1.0, |top| {
        ((STICKY_TOP_GAP - top) / STICKY_FADE).clamp(0.0, 1.0)
    });
    (push, lift)
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum ReviewItemAction {
    Seen {
        tab: Stream,
        path: String,
        near: usize,
    },
    Fold {
        path: String,
        /// Whether every changed line of the card is ticked seen.
        seen: bool,
    },
    Delete {
        id: u64,
    },
    Reply {
        tab: Stream,
        id: u64,
        near: usize,
    },
    /// The chip beside a text selection: comment on what is selected.
    Quote {
        tab: Stream,
        near: usize,
    },
    Outdated {
        path: String,
    },
    Gutter {
        tab: Stream,
        row: usize,
    },
    Expand {
        tab: Stream,
        row: usize,
    },
    Reveal {
        tab: Stream,
        row: usize,
    },
    Post,
    Cancel,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.DiffLine
    use mod.widgets.Gutter
    use mod.widgets.CollapsedRun
    use mod.widgets.CardCap
    use mod.widgets.CardCapBottom
    use mod.widgets.DropShadow
    use mod.widgets.ShadowUp
    use mod.widgets.FONT
    use mod.widgets.FONT_BOLD
    use mod.widgets.SeenBox
    use mod.widgets.DarkInput
    use mod.widgets.BarButton
    use mod.widgets.C_BG
    use mod.widgets.C_CARD
    use mod.widgets.C_BORDER
    use mod.widgets.C_TEXT
    use mod.widgets.C_DIM
    use mod.widgets.C_FAINT
    use mod.widgets.C_YELLOW
    use mod.widgets.C_ACCENT
    use mod.widgets.C_ON_ACCENT
    use mod.widgets.C_DELETED
    use mod.widgets.C_ELEMENT_HOVER

    // The composer's field is the box itself, not a well inside it: no border,
    // no inset fill, the prompt is the placeholder. With `height: Fit` it would
    // draw one line tall and only take clicks on that line (a TextInput
    // hit-tests its background box), so the field has to be the box it looks
    // like.
    let FlatInput = DarkInput {
        height: 60
        padding: 0
        // Room for a paragraph, and a comment that outgrows the box scrolls
        // inside it. Enter still posts, per `submit_on_enter`; Shift+Enter is
        // what breaks a line.
        is_multiline: true
        submit_on_enter: true
        draw_bg +: { border_size: 0.0 }
        // The draft reads as text and the prompt as a hint, in every state.
        // `get_color` mixes toward the state colours, and any we leave unset
        // fall through to makepad's own theme instead of ours — an unset
        // `color_focus` is why a focused field once drew its draft in makepad
        // grey.
        draw_text +: {
            color: mod.app_theme.color_text
            color_hover: mod.app_theme.color_text
            color_focus: mod.app_theme.color_text
            color_down: mod.app_theme.color_text
            color_empty: mod.app_theme.color_dim
            color_empty_hover: mod.app_theme.color_dim
            color_empty_focus: mod.app_theme.color_dim
            text_style: FONT{font_size: 9}
        }
    }

    // Authored text — prose, titles, comment bodies, paths — as opposed to
    // chrome. It is content, so you can select and copy it, and it stops
    // claiming pointer hits so the list can run a selection across it (see
    // `Label::selectable`). Every content label in a row is one of these, and
    // the row's text selects as one run.
    let Prose = Label {
        selectable: true
        draw_selection.color: mod.app_theme.color_sel_focus
    }

    // The card header's toggle for the conversations this range cannot place.
    // Hidden until there are some. The card header and its pinned twin share
    // it, since either can be the one on screen. Every colour state is set;
    // unset ones fall through to makepad's own theme instead of ours.
    let OutdatedToggle = ButtonFlatter {
        visible: false
        width: Fit height: Fit
        padding: Inset{top: 3 bottom: 3 left: 6 right: 6}
        margin: Inset{right: 4}
        draw_bg +: { color_hover: C_ELEMENT_HOVER border_radius: 4.0 }
        draw_text +: {
            color: C_FAINT
            color_hover: C_TEXT
            color_down: C_TEXT
            color_focus: C_FAINT
            color_disabled: C_FAINT
            text_style: FONT{font_size: 8.25}
        }
    }

    // A row inside a file card: the card's 16pt inset and its side borders,
    // which the code, the collapsed runs and the air between them carry
    // straight down from the header to the bottom cap.
    let CardSides = View {
        width: Fill height: Fit
        padding: Inset{left: 16 right: 16}
        sides := SolidView {
            width: Fill height: Fit
            draw_bg.color: C_BORDER
            padding: Inset{left: 1 right: 1}
        }
    }

    // One code row's content: the gutter rail and the line. Shared by the
    // card-framed `Code` and the bare `CodeFlat`, so the two never drift.
    let CodeRow = SolidView {
        width: Fill height: Fit
        draw_bg.color: C_CARD
        flow: Right
        gut := Gutter {}
        dl := DiffLine {}
    }

    // The strip a comment sits in: hairlines round it, the blue bar down its
    // left edge, the window-dark body. Posted comments and the composer share
    // it. Every level is named, so a user can reach the innermost box: a `+:`
    // merge resolves a direct child, and each level in between has to be
    // addressable for the nested form to get there.
    let StripFrame = View {
        width: Fill height: Fit
        padding: Inset{left: 16 right: 16}
        cm_frame := SolidView {
            width: Fill height: Fit
            draw_bg.color: C_BORDER
            // Hairlines above and below as well: the strip is its own box
            // inside the card, like the design draws it.
            padding: Inset{top: 1 bottom: 1 left: 1 right: 1}
            cm_bar := SolidView {
                width: Fill height: Fit
                draw_bg.color: C_ACCENT
                padding: Inset{left: 6}
                cm_strip := SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BG
                    padding: Inset{top: 12 bottom: 12 left: 12 right: 12}
                }
            }
        }
    }

    // A comment strip, shared by `Comment` and its indented `Reply` twin.
    // Hoisted so the two differ in one line — see the entries in the list.
    let CommentStrip = StripFrame {
        cm_frame +: { cm_bar +: { cm_strip +: {
            flow: Right
            align: Align{x: 0.0, y: 0.0}
            View {
                width: Fill height: Fit
                flow: Down
                spacing: 4
                // Who said it, and where it came from when that is not
                // here. Empty on the pre-author records, and an empty
                // label draws nothing.
                cm_meta := Prose {
                    width: Fill
                    draw_text.color: C_FAINT
                    draw_text.text_style: FONT{font_size: 8}
                }
                // What the comment is about, when it was left on a
                // selection rather than on whole lines. Quoted from
                // the content, so it reads as the code's words and not
                // as the reviewer's; hidden when there is none.
                cm_quote := Prose {
                    visible: false
                    width: Fill
                    draw_text.color: C_DIM
                    draw_text.text_style: FONT{font_size: 8.5}
                }
                cm_body := Prose {
                    width: Fill
                    draw_text.color: C_TEXT
                    draw_text.text_style: FONT{font_size: 9}
                }
            }
            cm_reply := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/reply.svg")
            }
            cm_delete := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/close_circle.svg")
            }
        } } }
    }

    // The fold caret in a card header: 12px, one per direction (the icon is
    // a resource, so open/shut is a visibility swap, not a swapped svg).
    let FoldCaret = ButtonFlatter {
        width: 12 height: 12
        padding: 0
        margin: 0
        text: ""
        icon_walk: Walk{width: 12 height: Fit}
        draw_icon +: {
            color: C_FAINT
            color_hover: C_TEXT
        }
    }

    // A file card's header box: fold caret, viewed tick box, path, ±stats.
    // The nested views fake a 1px border with rounded top corners. The card
    // header and its pinned copy are both this box: the copy rides on the
    // real header while it climbs, so any difference between the two reads
    // as the header changing shape mid-scroll. The levels down to the path
    // are named so the copy can reach it (see `CommentStrip`).
    let HeaderCard = View {
        width: Fill height: Fit
        flow: Overlay
        fh_frame := SolidView {
            width: Fill height: Fit
            draw_bg.color: C_BORDER
            padding: Inset{top: 1 left: 1 right: 1}
            fh_card := SolidView {
                width: Fill height: Fit
                draw_bg.color: C_CARD
                flow: Down

                fh_row := View {
                    width: Fill height: 30
                    flow: Right
                    spacing: 8
                    align: Align{x: 0.0, y: 0.5}
                    padding: Inset{left: 8 right: 8}

                    fold_button := FoldCaret {
                        draw_icon.svg: crate_resource("self:resources/icons/caret_down.svg")
                    }
                    unfold_button := FoldCaret {
                        visible: false
                        // A right caret's ink is 1:2, and the svg scales from
                        // its ink bounds, not the viewbox: ask for 6 wide so
                        // Fit lands on 12 tall.
                        icon_walk: Walk{width: 6 height: Fit}
                        draw_icon.svg: crate_resource("self:resources/icons/caret_right.svg")
                    }
                    seen_box := SeenBox {}
                    fh_path := Prose {
                        width: Fit
                        draw_text.color: C_TEXT
                        draw_text.text_style: FONT{font_size: 9}
                    }
                    View { width: Fill height: Fit }
                    // Conversations this range cannot place. Shown only when
                    // there are some, because a card with nothing hidden has
                    // nothing to offer.
                    fh_outdated := OutdatedToggle {}
                    fh_stat := Label {
                        width: Fit
                        draw_text.color: C_FAINT
                        draw_text.text_style: FONT{font_size: 9}
                    }
                }
                // The hairline under the title, dropped while the card is
                // folded (the CardEnd cap closes it instead).
                fh_rule := SolidView {
                    width: Fill height: 1
                    draw_bg.color: C_BORDER
                }
            }
        }
        // Rounds the card's top corners over the square body.
        View {
            width: Fill height: Fill
            align: Align{x: 0.0, y: 0.0}
            CardCap {}
        }
        // Only the pinned copy shows this: free-floating, it rounds at the
        // bottom too.
        fh_bottom := View {
            visible: false
            width: Fill height: Fill
            align: Align{x: 0.0, y: 1.0}
            CardCapBottom {}
        }
    }

    mod.widgets.ReviewList = #(ReviewList::register_widget(vm)) {
        width: Fill
        height: Fill
        flow: Down

        // The view's own controls, over the rows and never among them: a
        // stream with file cards shows it always, any other only while
        // finding. Shown and filled by ReviewList::draw_walk.
        tab_bar := mod.widgets.TabBar {
            visible: false
            // Collapse seen, off and on: the same icon, lit while it is on.
            // `DrawSvg` scales a path by its extent, and the chevrons are
            // tall and narrow: 7 wide comes out the size of the arrows.
            collapse_seen := BarButton {
                icon_walk: Walk{width: 7 height: Fit}
                draw_icon.svg: crate_resource("self:resources/icons/chevrons_down_up.svg")
            }
            show_seen := BarButton {
                visible: false
                icon_walk: Walk{width: 7 height: Fit}
                draw_icon +: {
                    svg: crate_resource("self:resources/icons/chevrons_down_up.svg")
                    color: C_ACCENT
                    color_hover: C_ACCENT
                }
            }
            find_row := mod.widgets.FindRow {
                visible: false
                margin: Inset{left: 8}
                replace_box +: { visible: true }
            }
            View { width: Fill height: Fit }
            bar_totals := Label {
                text: ""
                margin: Inset{right: 4}
                draw_text.color: C_FAINT
                draw_text.text_style: FONT{font_size: 8.25}
            }
            hunk_prev := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/arrow_up.svg")
            }
            hunk_next := BarButton {
                draw_icon.svg: crate_resource("self:resources/icons/arrow_down.svg")
            }
        }
        bar_rule := SolidView {
            visible: false
            width: Fill height: 1
            draw_bg.color: C_BORDER
        }
        bar_tooltip: mod.widgets.Tooltip {
            content +: {
                padding: 6
                draw_bg +: {
                    color: C_CARD
                    border_color: C_BORDER
                }
                tooltip_label +: {
                    width: Fit
                    draw_text +: {text_style: FONT{font_size: 8.25} color: C_TEXT}
                }
            }
        }

        body := View {
            width: Fill
            height: Fill
            flow: Overlay

            list := PortalList {
                width: Fill
                height: Fill
                flow: Down
                // One selection spanning prose and code rows.
                selectable: true
                drag_scrolling: false

                Title := View {
                    width: Fill height: Fit
                    padding: Inset{top: 16 bottom: 0 left: 16 right: 16}
                    title_text := Prose {
                        width: Fill height: Fit
                        padding: 0
                        draw_text.color: C_TEXT
                        draw_text.text_style: FONT_BOLD{font_size: 11 line_spacing: 1.45}
                    }
                }

                ProseRow := View {
                    width: Fill height: Fit
                    padding: Inset{top: 6 bottom: 6 left: 16 right: 16}
                    // Raw Markdown on purpose: Makepad's Markdown widget reparses
                    // the body on every scrolling redraw.
                    prose_text := Prose {
                        width: Fill height: Fit
                        padding: 0
                        draw_text.color: C_TEXT
                        draw_text.text_style: FONT{font_size: 9 line_spacing: 1.5}
                    }
                }

                // An unresolved agent reference. Loud on purpose.
                Warning := RoundedView {
                    width: Fill height: Fit
                    margin: Inset{top: 6 bottom: 6 left: 16 right: 16}
                    padding: Inset{top: 10 bottom: 10 left: 12 right: 12}
                    draw_bg.color: C_CARD
                    draw_bg.border_radius: 2.0
                    draw_bg.border_size: 1.0
                    draw_bg.border_color: C_YELLOW
                    warn_label := Prose {
                        width: Fill
                        draw_text.color: C_YELLOW
                        draw_text.text_style: FONT{font_size: 9}
                    }
                }

                // The top of a file card. The code rows below continue the side
                // borders and CardEnd closes the bottom — a "card" over a flat
                // virtualized list. The title row is a fixed 32, like the design:
                // a Fit height paints short of the box it lays out and left a band
                // of window background between the header and the first code row.
                // Fixed also because the body is hidden while the pinned copy
                // stands in for it, and a Fit box would collapse to nothing — the
                // row leaves the list, everything shunts up, the scroll resets. 16
                // lead-in + 1 border + 30 row + 1 rule.
                FileHeader := View {
                    width: Fill height: 48
                    padding: Inset{top: 16 bottom: 0 left: 16 right: 16}
                    flow: Overlay
                    fh_body := HeaderCard {}
                }

                // A collapsed run of unchanged lines: the arrow band(s) that reveal
                // it. Wrapped like a code row so the card's side borders carry
                // straight through the cut.
                Skipped := CardSides {
                    sides +: { collapsed := CollapsedRun {} }
                }

                // What the range removed, where it was removed from. The file view
                // shows the file as it is, so this stands in for lines the head
                // does not have — press it and they take its place.
                Removed := CardSides {
                    sides +: {
                        SolidView {
                            width: Fill height: Fit
                            draw_bg.color: C_CARD
                            flow: Right
                            align: Align{y: 0.5}
                            // Lines up with the gutter's line-number column, so the
                            // caret reads as belonging to the code's left edge.
                            padding: Inset{left: 6 top: 1 bottom: 1}
                            rm_button := ButtonFlatter {
                                width: Fit height: Fit
                                padding: Inset{top: 2 bottom: 2 left: 4 right: 6}
                                spacing: 4
                                icon_walk: Walk{width: 6 height: Fit}
                                draw_icon +: {
                                    color: C_DELETED
                                    color_hover: C_TEXT
                                    svg: crate_resource("self:resources/icons/caret_right.svg")
                                }
                                draw_bg +: { color_hover: C_ELEMENT_HOVER border_radius: 4.0 }
                                draw_text +: {
                                    color: C_DELETED
                                    color_hover: C_TEXT
                                    color_down: C_TEXT
                                    color_focus: C_DELETED
                                    color_disabled: C_DELETED
                                    text_style: FONT{font_size: 8.25}
                                }
                            }
                        }
                    }
                }

                // 10pt of air between the card's code and the chrome bracketing it.
                // Part of the code region, not of the chrome: a card ticked seen
                // runs its marker through this, and stops at a collapsed run.
                Spacer := CardSides {
                    sides +: {
                        SolidView {
                            width: Fill height: 10
                            draw_bg.color: C_CARD
                            flow: Right
                            sp_mark := SolidView {
                                width: 2 height: Fill
                                visible: false
                                draw_bg.color: C_ACCENT
                            }
                        }
                    }
                }

                // Hunk boundaries carry review-state anchors but draw nothing:
                // the design shows hunks as plain line-number jumps.
                HunkBar := View {
                    width: Fill height: 0
                }

                // A review comment, below the last line of its range and part of
                // the code flow, like the design: a full-width strip on the
                // window-dark tone, the blue comment bar running down its left edge
                // — no card, no range header. The bar tells you the range; the
                // byline has to tell you who, and in a thread that is the part that
                // must stay legible. Reply and delete are the quiet icons.
                //
                // `Reply` is the same strip, indented on the inner box's padding:
                // the outer 16 is the card's edge, and the accent bar would paint
                // into a margin.
                Comment := CommentStrip {}
                Reply := CommentStrip {
                    cm_frame +: { cm_bar +: { cm_strip +: {
                        padding: Inset{top: 12 bottom: 12 left: 36 right: 12}
                    } } }
                }

                // The row itself: the rail and the line, and nothing about the
                // box around them. Hoisted so a card and an editor can differ in
                // their framing without differing in their content.
                Code := CardSides {
                    sides +: { CodeRow {} }
                }
                // The same row in a File tab, which is an editor and not a code
                // block: no card sides, no inset, the text against the pane.
                CodeFlat := CodeRow {}

                // One line of authored text — a prompt, a response, a commit
                // message. The same rail and line a code row gets, on the page
                // instead of in a card, so the transcript reads as prose and still
                // selects, marks and comments exactly like code.
                ProseLine := View {
                    width: Fill height: Fit
                    padding: Inset{left: 16 right: 16}
                    CodeRow {
                        draw_bg.color: C_BG
                        gut +: { prose: true }
                        dl +: { prose: true }
                    }
                }

                // The inline comment composer, spliced below the last selected line
                // — the same strip a posted comment gets, so the blue bar ties it
                // to the selected range the same way. One box and two icons, like
                // the design: no well, no title, the placeholder names the target
                // range, send and dismiss are the icons.
                Composer := StripFrame {
                    cm_frame +: { cm_bar +: { cm_strip +: {
                        flow: Down
                        spacing: 10

                        comp_input := FlatInput {
                            width: Fill
                            empty_text: "Add a comment"
                        }
                        View {
                            width: Fill height: Fit
                            flow: Right
                            spacing: 1
                            align: Align{x: 1.0, y: 0.5}
                            comp_cancel := BarButton {
                                draw_icon.svg: crate_resource("self:resources/icons/close_circle.svg")
                            }
                            SolidView {
                                width: 1 height: 16
                                margin: Inset{left: 4 right: 4}
                                draw_bg.color: C_BORDER
                            }
                            comp_post := BarButton {
                                draw_icon.svg: crate_resource("self:resources/icons/send.svg")
                            }
                        }
                    } } }
                }

                // Bottom cap: rounds off the card the FileHeader opened. Its
                // bottom padding is the other half of the design's 32 between
                // cards (the next FileHeader brings 16 of its own).
                CardEnd := View {
                    width: Fill height: Fit
                    padding: Inset{left: 16 right: 16 bottom: 16}
                    flow: Overlay
                    SolidView {
                        width: Fill height: Fit
                        draw_bg.color: C_BORDER
                        padding: Inset{bottom: 1 left: 1 right: 1}
                        SolidView {
                            width: Fill height: 8
                            draw_bg.color: C_CARD
                        }
                    }
                    // … and its bottom corners. Offset by hand rather than aligned
                    // to the bottom of a Fill wrapper: inside this Fit parent the
                    // wrapper collapsed to the cap's own height, so "bottom" was
                    // the top and the cap's border landed 3px above the card's,
                    // leaving an unbordered lip that read as a drop shadow.
                    CardCapBottom {
                        margin: Inset{top: 3}
                    }
                }
            }

            // The offer to comment on selected text, parked at the end of the
            // selection. It is the selection's own affordance — the gutter's `+`
            // comments on whole lines, this one on the words — so it appears with
            // the selection and goes when the composer it opened does.
            quote_chip := View {
                visible: false
                width: Fit height: Fit
                quote_button := ButtonFlatter {
                    width: Fit height: Fit
                    padding: Inset{top: 4 bottom: 4 left: 8 right: 8}
                    spacing: 5
                    text: "Comment"
                    icon_walk: Walk{width: 11 height: Fit}
                    draw_icon +: {
                        color: C_ON_ACCENT
                        color_hover: C_ON_ACCENT
                        svg: crate_resource("self:resources/icons/comment.svg")
                    }
                    draw_bg +: {
                        color: C_ACCENT
                        color_hover: C_ACCENT
                        color_down: C_ACCENT
                        border_radius: 4.0
                    }
                    draw_text +: {
                        color: C_ON_ACCENT
                        color_hover: C_ON_ACCENT
                        color_down: C_ON_ACCENT
                        color_focus: C_ON_ACCENT
                        color_disabled: C_ON_ACCENT
                        text_style: FONT{font_size: 8.25}
                    }
                }
            }

            // The tab strip's shadow over the content. Drawn before the pinned
            // header so the header sits in front of it, the way it does in the
            // design: the strip's shadow is clipped short by the header's own top.
            DropShadow {}

            // The pinned copy of the card header whose rows fill the top of the
            // viewport: the header scrolls along, so the path and the viewed tick
            // box stay at hand until the card is read (and ticked) to its end.
            // Populated and shown by ReviewList::draw_walk.
            sticky := View {
                visible: false
                width: Fill height: Fit
                padding: Inset{left: 16 right: 16}
                flow: Down
                // The header's shadow reaching up into the gap — `0px 0px` in the
                // design, no Y offset — so it mixes with the tab strip's and the
                // gap gets dark enough that rows stop flashing through it. Always
                // laid out, only its opacity ramps: toggling `visible` moved the
                // whole box by its own height at one scroll position, and that jump
                // read as a flicker.
                st_shadow_up := ShadowUp {}
                // Chrome, not content: the copy's path is not selectable.
                HeaderCard {
                    fh_frame +: { fh_card +: { fh_row +: { fh_path +: {
                        selectable: false
                    } } } }
                    fh_bottom +: { visible: true }
                }
                // …and the header's own shadow onto the rows passing beneath it,
                // ramped by the same `fade`: at rest the copy rides on the real
                // header, which is part of the card and casts nothing.
                st_shadow := DropShadow {
                    width: Fill height: 12
                    strength: 0.84
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct ReviewList {
    #[deref]
    view: View,
    /// What a tab bar button does, shown while the pointer rests on it.
    #[live]
    bar_tooltip: Tooltip,
    /// Which bar button the tooltip names, by its place in `hover_bar`'s list.
    #[rust]
    bar_tip: Option<usize>,
    /// The window these rows belong to, taken from `Scope` — the pane above
    /// puts it there. Cached because the helpers below run off the event that
    /// carried it.
    #[rust]
    state: Option<std::sync::Arc<crate::window::WindowState>>,
    /// Which row stream this instance renders (`@review`/`@files`/
    /// `@sessions`/`@commits`): every dock tab owns one stream-pinned list.
    #[live]
    kind: LiveId,
    /// The header mirrored in this draw; action identity is the file path.
    #[rust]
    sticky_idx: Option<usize>,
    /// The composer field's widget, while one is on screen. Every other control
    /// in a row carries the `ReviewItemAction` that says what it is, but
    /// makepad's `TextInput` has no `#[action_data]` field — `set_action_data`
    /// on it is a no-op — so its keystrokes arrive unlabelled and `ReviewPane`
    /// matches them by uid instead. Uids come off a counter and are never
    /// reused, so a stale one is inert rather than wrong.
    #[rust]
    pub composer_input: Option<WidgetUid>,
    #[rust]
    composer: TextInputRef,
    /// Stream index per list entry while cards are folded shut. Empty means
    /// the identity mapping — no card is folded, the common case.
    #[rust]
    visible: Vec<usize>,
    #[rust]
    requested_highlights: HashSet<(u32, u64)>,
    /// The document generation, comments revision and row-stream revision
    /// this instance's caches were built against. The row stream counts on its
    /// own: splicing the composer inserts a row mid-stream, which shifts every
    /// row index after it — so `cards` (card boundaries by row) and `visible`
    /// (entry→row) are stale, and every card below the composer resolved to
    /// the wrong geometry: its header stopped sticking.
    #[rust]
    mapped: (u64, u64, u64),
    #[rust]
    mapped_folded: HashSet<String>,
    #[rust]
    mapped_entries: usize,
    #[rust]
    mapped_seen: Option<Arc<HashSet<LineKey>>>,
    /// File-card boundaries and changed-line keys, rebuilt only when the row
    /// stream changes. Sticky drawing must stay proportional to the viewport.
    #[rust]
    cards: CardIndex,
    /// Where the row that owns the caret drew it, read back after that row
    /// drew. `None` when the caret is off screen or on another stream.
    #[rust]
    caret_rect: Option<Rect>,
    /// Every code row this instance last drew, with the window band it
    /// occupies: `(row, top, bottom)`, in draw order. This is how a pointer
    /// position names a row without assuming all rows are the same height —
    /// they are not, as soon as a long line wraps.
    #[rust]
    drawn_rows: Vec<DrawnRow>,
    /// One-shot: put the keyboard in the find field on its next draw.
    #[rust]
    focus_find: bool,
    /// What the find bar is looking for, while it is open. Held by the view
    /// and not the document: a search is something you are doing, not
    /// something the review says.
    #[rust]
    find: Option<Find>,
    #[rust]
    find_at: Option<(usize, usize)>,
    /// A row to scroll to on the next draw, and how far below the top of the
    /// view to put it.
    #[rust]
    seek_row: Option<(usize, f64)>,
    /// Where the selection ended this pass and the row it ended on: what the
    /// chip is placed against, and what the comment it opens anchors to.
    #[rust]
    selection_end: Option<(Rect, usize)>,
    /// The stream this instance last drew. `tab_of` walks the widget tree,
    /// which only resolves while drawing; asked during event handling, every
    /// instance fell back to the same answer, and gating a keystroke on that
    /// applied it once per instance. So it is captured here.
    #[rust]
    drawn_tab: Option<Stream>,
    /// Whether the pointer gesture in flight went down inside this list — see
    /// [`ReviewList::owns_gesture`].
    #[rust]
    pressed_inside: bool,
}

#[derive(Default)]
struct CardIndex {
    cards: Vec<CardMeta>,
}

struct CardMeta {
    header: usize,
    end: usize,
    keys: Vec<LineKey>,
    seen: Cell<Option<Seen>>,
}

impl CardIndex {
    fn rebuild(&mut self, rows: &[Row], blobs: &[Blob]) {
        self.cards.clear();
        for (row, value) in rows.iter().enumerate() {
            match value {
                Row::FileHeader { .. } => self.cards.push(CardMeta {
                    header: row,
                    end: row,
                    keys: Vec::new(),
                    seen: Cell::new(None),
                }),
                Row::HunkBar { old, new } => {
                    if let Some(card) = self.cards.last_mut() {
                        card.keys.extend(store::hunk_keys(*old, *new, blobs));
                    }
                }
                Row::CardEnd => {
                    if let Some(card) = self.cards.last_mut() {
                        card.end = row;
                    }
                }
                // Neither a card boundary nor review keys.
                Row::Title { .. }
                | Row::Prose { .. }
                | Row::Warning { .. }
                | Row::Collapsed { .. }
                | Row::Removed { .. }
                | Row::Code { .. }
                | Row::Comment { .. }
                | Row::Spacer
                | Row::Composer => {}
            }
        }
    }

    fn containing(&self, row: usize) -> Option<&CardMeta> {
        let card = self
            .cards
            .partition_point(|card| card.header <= row)
            .checked_sub(1)
            .and_then(|index| self.cards.get(index))?;
        (row <= card.end).then_some(card)
    }

    fn at(&self, header: usize) -> Option<&CardMeta> {
        self.cards
            .binary_search_by_key(&header, |card| card.header)
            .ok()
            .and_then(|index| self.cards.get(index))
    }

    fn invalidate_seen(&mut self) {
        for card in &mut self.cards {
            card.seen.set(None);
        }
    }
}

impl CardMeta {
    fn seen_state(&self, seen: &HashSet<LineKey>) -> Seen {
        if let Some(state) = self.seen.get() {
            return state;
        }
        let state = store::seen_state(&self.keys, seen);
        self.seen.set(Some(state));
        state
    }
}

impl WidgetMatchEvent for ReviewList {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions, _scope: &mut Scope) {
        self.handle_find(cx, actions);
        self.handle_hunk_buttons(cx, actions);
        // Collapse seen is one per window: every stream of it shuts the same
        // cards.
        let on = self.view.button(cx, ids!(collapse_seen)).clicked(actions);
        if on || self.view.button(cx, ids!(show_seen)).clicked(actions) {
            self.state().with(|d| {
                d.collapse_seen = on;
                d.unfolded.clear();
            });
            self.bar_tooltip.hide(cx);
            self.redraw(cx);
        }
    }
}

impl ReviewList {
    /// The window these rows belong to. Detached until the first event carries
    /// one down from the pane, which is before any gesture can reach a row.
    fn state(&self) -> &std::sync::Arc<crate::window::WindowState> {
        self.state
            .as_ref()
            .unwrap_or_else(|| crate::window::WindowState::detached())
    }

    /// The stream row a list entry renders. Folding hides rows, so an entry
    /// index is not a row index while any card is shut.
    pub fn row_at(&self, entry: usize) -> Option<usize> {
        if self.visible.is_empty() {
            Some(entry)
        } else {
            self.visible.get(entry).copied()
        }
    }

    /// The list entry a stream row renders as, the inverse of [`Self::row_at`].
    /// `None` while the row's card is shut.
    fn entry_of(&self, row: usize) -> Option<usize> {
        if self.visible.is_empty() {
            Some(row)
        } else {
            self.visible.binary_search(&row).ok()
        }
    }

    /// The tab bar's arrows: scroll to the hunk before or after the one at
    /// the top of the view. A shut card's hunks are not in the view, so they
    /// are skipped.
    fn handle_hunk_buttons(&mut self, cx: &mut Cx, actions: &Actions) {
        let forward = self.view.button(cx, ids!(hunk_next)).clicked(actions);
        if !forward && !self.view.button(cx, ids!(hunk_prev)).clicked(actions) {
            return;
        }
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let top = self.view.portal_list(cx, ids!(list)).first_id();
        let Some(from) = self.row_at(top) else {
            return;
        };
        let shown = |row: usize| self.entry_of(row).is_some();
        let target = self.state().read(|d| {
            let rows = d.stream(tab);
            if forward {
                first_hunk(rows, from + 1..rows.len(), shown)
            } else {
                first_hunk(rows, (0..from).rev(), shown)
            }
        });
        if let Some(row) = target {
            // Below the pinned header, which would otherwise cover the hunk.
            self.seek_row = Some((row, STICKY_TOP_GAP + STICKY_HEIGHT));
            self.redraw(cx);
        }
    }

    /// Rebuild the entry→row mapping for this stream's fold state. A folded
    /// card keeps its header and its bottom cap; everything between them —
    /// hunks, comments, the composer — stops being an entry at all, so the
    /// list never walks (or measures) the rows it isn't drawing.
    // NOTE: every row between a header and its cap is the card's body, a new kind too.
    // coland-ignore: @coland/absorbing_default_branch
    fn map_folded(&mut self, rows: &[Row], folded: &HashSet<String>) {
        self.visible.clear();
        if folded.is_empty() {
            return;
        }
        let mut hidden = false;
        for (i, row) in rows.iter().enumerate() {
            match row {
                Row::FileHeader { path, .. } => {
                    hidden = folded.contains(path);
                    self.visible.push(i);
                }
                Row::CardEnd => {
                    hidden = false;
                    self.visible.push(i);
                }
                _ if hidden => {}
                _ => self.visible.push(i),
            }
        }
    }
}

/// The first of `candidates` that starts a hunk the view shows.
fn first_hunk(
    rows: &[Row],
    mut candidates: impl Iterator<Item = usize>,
    shown: impl Fn(usize) -> bool,
) -> Option<usize> {
    candidates.find(|&row| matches!(rows[row], Row::HunkBar { .. }) && shown(row))
}

/// Whether the pointer gesture that is ending belongs to the view laid out at
/// `rect`: its press went down inside it. `pressed` carries that answer from
/// the press to the release.
///
/// The release cannot answer on its own. `MouseUp` is delivered to every dock
/// item, hidden tabs included, so a view that only ever held the pointer once
/// would otherwise keep answering for gestures made somewhere else. The press
/// is visibility-gated by the dock, so it is the half of the gesture that
/// knows where it happened.
fn owns_gesture(pressed: &mut bool, event: &Event, rect: impl FnOnce() -> Rect) -> bool {
    match event {
        Event::MouseDown(e) => {
            *pressed = rect().contains(e.abs);
            false
        }
        Event::MouseUp(_) => std::mem::take(pressed),
        // A touch drag carries the selection with it, so the gesture stays the
        // pressed view's until the last finger lifts.
        Event::TouchUpdate(e) => {
            if let Some(touch) = e
                .touches
                .iter()
                .find(|touch| matches!(touch.state, TouchState::Start))
            {
                *pressed = rect().contains(touch.abs);
            }
            if e.touches
                .iter()
                .all(|touch| matches!(touch.state, TouchState::Stop))
            {
                std::mem::take(pressed)
            } else {
                *pressed
            }
        }
        _ => false,
    }
}

/// Whether the rows leading away from a collapsed run reach code before they
/// reach the card's chrome — that decides whether the end gets an expander. It
/// takes the stream in either direction, so one walk answers both sides. The
/// rows it steps over draw nothing between the run and the code: the hunk
/// anchor has no height, and the card's 10pt of air is the padding around the
/// code itself.
// NOTE: draw_row asks it for both ends of a run.
// coland-ignore: @coland/single_caller_private_function
fn reaches_code<'a>(mut rows: impl Iterator<Item = &'a Row>) -> bool {
    rows.find(|row| !matches!(row, Row::HunkBar { .. } | Row::Spacer))
        .is_some_and(|row| matches!(row, Row::Code { .. } | Row::Comment { .. } | Row::Composer))
}

/// The card header's right-hand stat, shared with the sticky copy.
// NOTE: fill_header's stat line, for the card and its sticky copy alike.
// coland-ignore: @coland/single_caller_private_function
fn header_stat(adds: usize, dels: usize, similarity: Option<u8>, partially_viewed: bool) -> String {
    let stat = match similarity {
        // 100% similar: nothing to review, so say so instead of +59/-59.
        Some(100) => "renamed, unchanged".to_string(),
        Some(s) => format!("renamed {s}%   +{adds} -{dels}"),
        None => format!("+{adds} -{dels}"),
    };
    if partially_viewed {
        format!("partially viewed · {stat}")
    } else {
        stat
    }
}

/// What the card header's toggle says: how much is hidden, or the way back.
// NOTE: fill_header's toggle text, beside header_stat.
// coland-ignore: @coland/single_caller_private_function
fn outdated_label(hidden: usize, showing: bool) -> String {
    if showing {
        "hide outdated".to_string()
    } else {
        format!(
            "{hidden} outdated conversation{}",
            if hidden == 1 { "" } else { "s" }
        )
    }
}

/// What every row of one draw pass reads: the frame, resolved once.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Wrapping, selection, and composer focus are independent draw properties."
)]
struct DrawPass<'a> {
    doc: &'a Arc<ReviewDoc>,
    /// `doc.stream(tab)`, resolved once: for a File tab that is a scan of the
    /// open files, and every row used to pay it four times.
    rows: &'a [Row],
    review: &'a ReviewState,
    tab: Stream,
    theme: FrameTheme,
    /// Whether the rows take their selection from the document this pass.
    ///
    /// The list paints what its own pointer gesture produced and nothing
    /// else, so a selection made any other way (extended with shift, set by
    /// a test hook) would be invisible. While the list has a gesture of its
    /// own it stays the authority: during a drag it is the live one, and the
    /// document has not adopted it yet.
    paint_selection: bool,
    focus_composer: bool,
    requested_composer: bool,
    draft: &'a str,
    /// Where the document's selection sits in this stream, resolved once: per
    /// row it is a lookup, and resolving it per row would walk the stream for
    /// every line on screen.
    selected: Option<crate::review_doc::Selected>,
    /// The font's wrap setting, read once rather than per row.
    wrap: bool,
    /// The window a highlight request answers to: the highlighter runs off
    /// the UI thread, so the request carries both the document and the window.
    window: LiveId,
}

/// The list's entry range for one pass, from the caches [`ReviewList::refresh_caches`]
/// just brought up to date.
struct EntryRange {
    /// Rows or folds changed since the last pass: every index-keyed cache
    /// was rebuilt, and the list's has to be too.
    remapped: bool,
    /// How many entries the last pass had.
    previous: usize,
    entries: usize,
    /// The entry the viewport starts on, under the new mapping.
    first: usize,
    /// The entry it started on before this pass.
    old_first: usize,
}

/// Give the list its range. `PortalList` caches heights by entry index, while
/// folding changes which document row an entry index names, so a remap resets
/// the old measurements with a one-entry shrink before growing the new range.
// NOTE: PortalList's range protocol (shrink before a remap), apart from the draw.
// coland-ignore: @coland/single_caller_private_function
fn set_range(cx: &mut Cx2d, list: &mut PortalList, range: &EntryRange) {
    if range.remapped && range.previous != 0 && range.entries >= range.previous {
        list.set_item_range(cx, 0, range.previous - 1);
    }
    list.set_item_range(cx, 0, range.entries);
    // Re-anchor only when the entry the viewport starts on actually moved. A
    // fold remaps entries and moves it; a stream re-lowered in place does not,
    // and re-anchoring then would also reset `first_scroll` to zero — a
    // visible jump of up to one row on every keystroke once these rows are
    // editable.
    if range.remapped && range.entries != 0 && range.first != range.old_first {
        list.set_first_id_and_scroll(range.first.min(range.entries - 1), 0.0);
    }
}

impl Widget for ReviewList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(frame) = scope.data.get::<FrameData>() else {
            return DrawStep::done();
        };
        // Also taken here, not only from the event path: the first draw of a
        // window happens before any event reaches its rows, and the IME gate
        // at the end of this function needs to know whose window this is.
        self.state = Some(frame.state.clone());
        let tab = self.tab_of(cx, &frame.document.files_open);
        self.drawn_tab = Some(tab);
        if self.find.is_some()
            && (self.mapped.0 != frame.document.generation
                || self.mapped.2 != frame.document.rows_rev)
        {
            self.find_at = None;
            self.count_hits(cx);
        }
        let list_ref = self.view.portal_list(cx, ids!(list));
        let pass = DrawPass {
            doc: &frame.document,
            rows: frame.document.stream(tab),
            review: &frame.review,
            tab,
            theme: FrameTheme(frame.theme.clone()),
            paint_selection: !list_ref.borrow().is_some_and(|l| l.has_selection()),
            focus_composer: frame.focus_composer == Some(tab)
                || (frame.document.composer_tab == Some(tab)
                    && !matches!(self.composer.area(), Area::Empty)
                    && cx.has_key_focus(self.composer.area())),
            requested_composer: frame.focus_composer == Some(tab),
            draft: &frame.compose_draft,
            selected: crate::review_doc::selected(
                frame.document.stream(tab),
                frame.document.caret,
                frame.document.selection_anchor,
            ),
            wrap: crate::theme::active_font().wrap,
            window: frame.state.id,
        };
        let range = self.refresh_caches(&pass, list_ref.first_id());
        // Re-established by whichever row draws the caret this pass; stale
        // geometry would aim the IME at a line that has scrolled away.
        self.caret_rect = None;
        self.selection_end = None;
        self.drawn_rows.clear();
        self.fill_bar(cx, &pass);

        // The list's own chrome — the pinned header's rounded caps, the top
        // fade — reads the theme off the scope like every row does. Drawing the
        // root with an empty one made each of them bail out silently.
        let mut list_scope = Scope::with_props(&pass.theme);
        while let Some(step) = self.view.draw_walk(cx, &mut list_scope, walk).step() {
            let (push, lift) = {
                let list = step.as_portal_list();
                let Some(mut list) = list.borrow_mut() else {
                    continue;
                };
                set_range(cx, &mut list, &range);
                let composer = pass
                    .requested_composer
                    .then(|| {
                        pass.rows
                            .iter()
                            .position(|row| matches!(row, Row::Composer))
                    })
                    .flatten();
                let composer = composer.map(|row| (row, 0.0));
                for (row, below) in [composer, self.seek_row.take()].into_iter().flatten() {
                    if let Some(entry) = self.entry_of(row) {
                        list.set_first_id_and_scroll(entry, below);
                    }
                }
                let mut drawn = Vec::new();
                while let Some(entry) = list.next_visible_item(cx) {
                    drawn.push(entry);
                    self.draw_row(cx, &mut list, entry, &pass);
                }
                self.place_sticky(&list, &drawn, &range, &pass)
            };
            self.draw_sticky(cx, &pass, push, lift);
            self.draw_chip(cx, &pass);
        }
        if std::mem::take(&mut self.focus_find) {
            let input = self.view.text_input(cx, ids!(find_input));
            input.set_text(cx, self.find.as_ref().map(Find::query).unwrap_or_default());
            input.set_key_focus(cx);
        }
        self.arm_ime(cx);
        self.bar_tooltip.draw_walk(cx, scope, Walk::default())
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Some(state) = crate::frame_state(scope) {
            self.state = Some(state);
        }
        // The caret's keys are claimed before the list sees them: `PortalList`
        // binds Home, Cmd-A and the arrows to its own scrolling and selection,
        // and the caret needs all of them. Makepad dispatch is explicit, so not
        // forwarding is what consumes a key.
        //
        // NOTE: the document is mutated here, never during a draw. `with_doc`
        // is `Arc::make_mut`, and a draw holds a second `Arc` through
        // `FrameData`, so an edit applied mid-draw would deep-clone the whole
        // document, blob texts included, on every keystroke.
        match event {
            // NOTE: Each tab claims search only while its list or find fields own focus.
            Event::KeyDown(ke) if self.find_keys(cx, ke) => return,
            Event::KeyDown(ke) if self.caret_keys(cx, ke) => return,
            Event::TextInput(ti) if self.caret_text(cx, &ti.input) => return,
            _ => {}
        }
        if self.pointer_overlays(cx, event, scope) {
            return;
        }
        self.view.handle_event(cx, event, scope);
        // ...and its position is read back afterwards, because the gesture that
        // moves it is the list's own drag. The list holds it in item indices,
        // which only name rows currently on screen; the document holds it in
        // blob coordinates, which outlive scrolling and resplicing.
        // Only the list the gesture went down in adopts what it produced: the
        // caret, the stream the composer splices into, and the key focus all
        // follow the pointer, not every list that still holds a selection.
        if owns_gesture(&mut self.pressed_inside, event, || {
            self.view.portal_list(cx, ids!(list)).area().rect(cx)
        }) {
            self.adopt_list_selection(cx);
        }
        self.widget_match_event(cx, event, scope);
    }
}

impl ReviewList {
    /// The tab bar for this pass: whether it shows, and which of its parts.
    /// A view of file cards keeps its bar; any other has one while finding.
    fn fill_bar(&self, cx: &mut Cx2d, pass: &DrawPass) {
        let cards = matches!(
            pass.tab,
            Stream::Guide | Stream::Files | Stream::Sessions | Stream::Commits
        );
        let finding = self.find.is_some();
        for bar in [ids!(tab_bar), ids!(bar_rule)] {
            self.view.view(cx, bar).set_visible(cx, cards || finding);
        }
        self.view.view(cx, ids!(find_row)).set_visible(cx, finding);
        let on = pass.doc.collapse_seen;
        self.view
            .button(cx, ids!(collapse_seen))
            .set_visible(cx, cards && !on);
        self.view
            .button(cx, ids!(show_seen))
            .set_visible(cx, cards && on);
        for card_only in [ids!(bar_totals), ids!(hunk_prev), ids!(hunk_next)] {
            self.view.widget(cx, card_only).set_visible(cx, cards);
        }
        let (adds, dels) = pass.doc.totals;
        self.view
            .label(cx, ids!(bar_totals))
            .set_text(cx, &format!("+{adds} -{dels}"));
    }

    /// What floats over the rows sees the pointer first. The quote chip takes
    /// it outright: dispatched to the whole tree in order, a press would land
    /// on the line underneath as well and clear the very selection the chip
    /// is offering to comment on. The tab bar's tooltip names the button the
    /// pointer rests on. Whether the chip took the event.
    fn pointer_overlays(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        let chip = self.view.view(cx, ids!(quote_chip));
        if chip.visible() && pointer_at(event).is_some_and(|at| chip.area().rect(cx).contains(at)) {
            chip.handle_event(cx, event, scope);
            return true;
        }
        self.bar_tooltip.handle_event(cx, event, scope);
        let Event::MouseMove(e) = event else {
            return false;
        };
        let tips = [
            (ids!(collapse_seen), "Collapse seen"),
            (ids!(show_seen), "Show seen"),
            (ids!(find_prev), "Previous match"),
            (ids!(find_next), "Next match"),
            (ids!(find_close), "Close find"),
            (ids!(hunk_prev), "Previous hunk"),
            (ids!(hunk_next), "Next hunk"),
        ];
        let over = tips.into_iter().enumerate().find_map(|(i, (id, tip))| {
            let button = self.view.button(cx, id);
            let rect = button.area().rect(cx);
            (button.visible() && rect.contains(e.abs)).then_some((i, rect, tip))
        });
        if over.map(|(i, ..)| i) != self.bar_tip {
            self.bar_tip = over.map(|(i, ..)| i);
            match over {
                Some((_, rect, tip)) => {
                    let below = dvec2(rect.pos.x, rect.pos.y + rect.size.y + 4.0);
                    self.bar_tooltip.show_with_options(cx, below, tip);
                }
                None => self.bar_tooltip.hide(cx),
            }
        }
        false
    }
}

/// Where a pointer event is, for the widgets that have to know whether it is
/// over them before anyone handles it.
// NOTE: one position per pointer event, for the chip's hit test in pointer_overlays.
// coland-ignore: @coland/single_caller_private_function
fn pointer_at(event: &Event) -> Option<DVec2> {
    match event {
        Event::MouseDown(e) => Some(e.abs),
        Event::MouseUp(e) => Some(e.abs),
        Event::MouseMove(e) => Some(e.abs),
        Event::TouchUpdate(e) => e.touches.first().map(|touch| touch.abs),
        _ => None,
    }
}

impl ReviewList {
    /// The stream this instance renders. The dock's content templates pin each
    /// list to one via `kind`.
    ///
    /// All fixed streams have their own kind, but there is one File pane per
    /// open file and they share `@file` — so a File pane finds out which file by
    /// looking for its own dock tab in the widget tree path, the way a terminal
    /// pane finds its session (`TerminalView::session_for_widget`). The tab is
    /// the pane's identity, so the walk happens once and the answer is reused
    /// until that file closes. A pane whose tab the document has no file for
    /// renders an empty stream.
    fn tab_of(&self, cx: &Cx, open: &[FileView]) -> Stream {
        if self.kind == id!(guide) {
            Stream::Guide
        } else if self.kind == id!(sessions) {
            Stream::Sessions
        } else if self.kind == id!(commits) {
            Stream::Commits
        } else if self.kind == id!(comments) {
            Stream::Comments
        } else if self.kind == id!(file) {
            if let Some(Stream::File(tab)) = self.drawn_tab
                && open.iter().any(|f| f.tab == tab)
            {
                return Stream::File(tab);
            }
            let path = cx.widget_tree().path_to(self.widget_uid());
            let tab = path
                .iter()
                .rev()
                .find(|node| open.iter().any(|f| f.tab == node.0));
            Stream::File(tab.map_or(0, |t| t.0))
        } else {
            Stream::Files
        }
    }

    /// Bring every index-keyed cache up to date with the document, and work
    /// out the list's entry range and where the viewport starts in it. Folded
    /// cards drop their rows from the list; a new row mapping also invalidates
    /// `PortalList`'s index-keyed height cache, which [`set_range`] resets.
    // NOTE: brings the row-keyed caches up to date on the way; the range is worked out, not read.
    // coland-ignore: @coland/read_write_operation
    fn refresh_caches(&mut self, pass: &DrawPass, old_first_id: usize) -> EntryRange {
        let d = pass.doc;
        if self.mapped.0 != d.generation {
            self.requested_highlights.clear();
        }
        // The composer moving is a change of the same kind as a comment landing:
        // a row appears or leaves mid-stream, so everything keyed by row index
        // has to be rebuilt.
        let key = (d.generation, pass.review.comments_rev, d.rows_rev);
        let changed = self.mapped != key;
        let previous = self.mapped_entries;
        let old_first_row = self.row_at(old_first_id).unwrap_or(0);
        if changed {
            self.cards.rebuild(pass.rows, &d.blobs);
        }
        if changed
            || self
                .mapped_seen
                .as_ref()
                .is_none_or(|seen| !Arc::ptr_eq(seen, &pass.review.seen))
        {
            self.cards.invalidate_seen();
            self.mapped_seen = Some(pass.review.seen.clone());
        }
        // Which cards are shut follows the seen ticks as well as the folds:
        // with Collapse seen on, ticking a card's last line shuts it.
        let shut: HashSet<String> = self
            .cards
            .cards
            .iter()
            .filter_map(|card| match &pass.rows[card.header] {
                Row::FileHeader { path, .. }
                    if card_shut(d, path, card.seen_state(&pass.review.seen) == Seen::All) =>
                {
                    Some(path.clone())
                }
                _ => None,
            })
            .collect();
        let mapping_changed = changed || self.mapped_folded != shut;
        if mapping_changed {
            self.map_folded(pass.rows, &shut);
            self.mapped = key;
            self.mapped_folded = shut;
        }
        let first = if self.visible.is_empty() {
            old_first_row
        } else {
            match self.visible.binary_search(&old_first_row) {
                Ok(entry) => entry,
                Err(0) => 0,
                Err(entry) => entry - 1,
            }
        };
        let entries = if self.visible.is_empty() {
            pass.rows.len()
        } else {
            self.visible.len()
        };
        self.mapped_entries = entries;
        EntryRange {
            remapped: mapping_changed,
            previous,
            entries,
            first,
            old_first: old_first_id,
        }
    }

    /// One list entry: the row it maps to, drawn with that row's template.
    #[expect(
        clippy::too_many_lines,
        reason = "The exhaustive row dispatch keeps each template beside its row data."
    )]
    // NOTE: the inner match's row is already one of the three label rows.
    // coland-ignore: @coland/absorbing_default_branch
    fn draw_row(&mut self, cx: &mut Cx2d, list: &mut PortalList, entry: usize, pass: &DrawPass) {
        let Some(r) = self.row_at(entry) else {
            return;
        };
        let Some(row) = pass.rows.get(r) else {
            return;
        };
        let (tab, theme) = (pass.tab, &pass.theme);
        match row {
            // One label each, in the row's own template.
            Row::Title { text } | Row::Prose { md: text } | Row::Warning { text } => {
                let (template, label) = match row {
                    Row::Title { .. } => (id!(Title), ids!(title_text)),
                    Row::Prose { .. } => (id!(ProseRow), ids!(prose_text)),
                    _ => (id!(Warning), ids!(warn_label)),
                };
                let item = list.item(cx, entry, template);
                item.label(cx, label).set_text(cx, text);
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            Row::FileHeader { .. } => {
                let item = list.item(cx, entry, id!(FileHeader));
                // Invisible while the pinned copy stands in for this card — and
                // inert with it, since an invisible View drops the events its
                // caret and tick box need. The row keeps its height, so nothing
                // moves.
                item.view(cx, ids!(fh_body))
                    .set_visible(cx, self.sticky_idx != Some(r));
                self.fill_header(cx, &item, r, pass);
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            // What the range took out, standing where it was. The file view
            // shows the file as it is, so this is the only trace of a deletion
            // until it is asked for.
            Row::Removed { start, end, .. } => {
                let item = list.item(cx, entry, id!(Removed));
                let n = end - start + 1;
                let plural = if n == 1 { "" } else { "s" };
                let button = item.button(cx, ids!(rm_button));
                button.set_text(cx, &format!("{n} line{plural} removed"));
                button.set_action_data(ReviewItemAction::Reveal { tab, row: r });
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            // A cut in the code, with its two expanders. The row says how many
            // lines are hidden and where they live; the band says which end a
            // click asked for.
            Row::Collapsed { .. } => {
                let item = list.item(cx, entry, id!(Skipped));
                item.widget(cx, ids!(collapsed))
                    .set_action_data(ReviewItemAction::Expand { tab, row: r });
                // An end is only reachable if there is code on that side to
                // grow: a run at the top of a card has none above it, one at
                // the bottom none below, and each gets one band instead of two.
                if let Some(mut s) = item
                    .widget(cx, ids!(collapsed))
                    .borrow_mut::<CollapsedRun>()
                {
                    s.head = reaches_code(pass.rows[..r].iter().rev());
                    s.tail = reaches_code(pass.rows[r + 1..].iter());
                }
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            // Air, but part of the code region: it carries the seen marker,
            // which stops at a collapsed run.
            Row::Spacer => {
                let item = list.item(cx, entry, id!(Spacer));
                let seen = self
                    .cards
                    .containing(r)
                    .is_some_and(|card| card.seen_state(&pass.review.seen) == Seen::All);
                item.view(cx, ids!(sp_mark)).set_visible(cx, seen);
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            Row::Code { blob, .. } => {
                self.request_highlight(*blob, pass);
                // Authored text renders on the page — a transcript entry is not
                // a file — and a File tab is an editor, so neither is in a card.
                let template = if pass.doc.blobs[*blob as usize].prose.is_some() {
                    id!(ProseLine)
                } else if self.kind == id!(file) {
                    id!(CodeFlat)
                } else {
                    id!(Code)
                };
                let item = list.item(cx, entry, template);
                self.draw_code(cx, &item, r, pass);
            }
            // Hunk boundaries render as nothing — the design shows hunks as
            // line-number jumps. The row still anchors the hunk's review-state
            // keys for the header tick box.
            Row::HunkBar { .. } => {
                let item = list.item(cx, entry, id!(HunkBar));
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            // A reply gets the indented twin of the same strip. `meta` is the
            // byline, not the range — the blue bar spanning the range already
            // tells that. The buttons carry the comment's id, not its row: a
            // resplice between the draw and the click renumbers rows.
            Row::Comment {
                id,
                parent,
                body,
                meta,
                quote,
            } => {
                let template = if parent.is_some() {
                    id!(Reply)
                } else {
                    id!(Comment)
                };
                let item = list.item(cx, entry, template);
                item.widget(cx, ids!(cm_reply))
                    .set_action_data(ReviewItemAction::Reply {
                        tab,
                        id: *id,
                        near: r,
                    });
                item.widget(cx, ids!(cm_delete))
                    .set_action_data(ReviewItemAction::Delete { id: *id });
                item.label(cx, ids!(cm_meta)).set_text(cx, meta);
                let quoted = item.label(cx, ids!(cm_quote));
                quoted.set_visible(cx, quote.is_some());
                if let Some(quote) = quote {
                    quoted.set_text(cx, &format!("“{quote}”"));
                }
                item.label(cx, ids!(cm_body)).set_text(cx, body);
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            Row::CardEnd => {
                let item = list.item(cx, entry, id!(CardEnd));
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            Row::Composer => {
                let item = list.item(cx, entry, id!(Composer));
                self.draw_composer(cx, &item, pass);
            }
        }
    }

    /// Lazy highlight: only blobs with a line actually on screen ever get
    /// parsed. This is what keeps a 850-file diff cheap. Every stream indexes
    /// the same blob table, so it works for each tab unchanged.
    fn request_highlight(&mut self, blob: u32, pass: &DrawPass) {
        let b = &pass.doc.blobs[blob as usize];
        // Keyed by rev as well as blob: an edit invalidates the spans of the
        // lines it touched, and asking again for the same blob has to get past
        // this memo. …and only for a blob the draw cannot colour itself.
        // Otherwise the worker computes a whole-file highlight that nothing
        // ever reads: `row_spans` answers from its own tree regardless of what
        // lands in `Blob.spans`.
        if !draws_own_spans(b)
            && b.spans_stale()
            && self.requested_highlights.insert((blob, b.edit_rev))
        {
            highlight().send(HighlightCmd {
                window: pass.window,
                doc: pass.doc.clone(),
                generation: pass.doc.generation,
                blob,
                rev: b.edit_rev,
            });
        }
    }

    /// The header of the card at row `r`, in the box under `root`: the list
    /// row's or the pinned copy's.
    fn fill_header(&self, cx: &mut Cx2d, root: &WidgetRef, r: usize, pass: &DrawPass) {
        let Some(Row::FileHeader {
            path,
            adds,
            dels,
            from,
            similarity,
            ..
        }) = pass.rows.get(r)
        else {
            return;
        };
        let d = pass.doc;
        root.widget(cx, ids!(seen_box))
            .set_action_data(ReviewItemAction::Seen {
                tab: pass.tab,
                path: path.clone(),
                near: r,
            });
        // The viewed tick box covers every hunk of this card — all their
        // changed lines flip together.
        let seen = self
            .cards
            .at(r)
            .map_or(Seen::Unseen, |card| card.seen_state(&pass.review.seen));
        for ids in [ids!(fold_button), ids!(unfold_button)] {
            root.widget(cx, ids)
                .set_action_data(ReviewItemAction::Fold {
                    path: path.clone(),
                    seen: seen == Seen::All,
                });
        }
        root.label(cx, ids!(fh_path))
            .set_text(cx, &header_title(path, from.as_deref()));
        root.check_box(cx, ids!(seen_box))
            .set_active(cx, seen == Seen::All, Animate::No);
        root.label(cx, ids!(fh_stat)).set_text(
            cx,
            &header_stat(*adds, *dels, *similarity, seen == Seen::Partly),
        );
        // Conversations recorded against this file that this range cannot
        // place. Offered only when there are some; the label says which way
        // the click goes. What the last comment splice managed to place is
        // read, not recomputed: the splice decided, and deciding again here is
        // how a header ends up disagreeing with the rows right under it.
        let hidden = store::outdated_threads(&pass.review.comments, path, &d.placed_threads);
        let outdated = root.button(cx, ids!(fh_outdated));
        outdated.set_visible(cx, hidden > 0);
        if hidden > 0 {
            outdated.set_text(
                cx,
                &outdated_label(hidden, d.show_all_comments.contains(path)),
            );
            root.widget(cx, ids!(fh_outdated))
                .set_action_data(ReviewItemAction::Outdated { path: path.clone() });
        }
        // Shut cards show the other caret and drop the hairline: the bottom
        // cap closes them instead.
        let folded = card_shut(d, path, seen == Seen::All);
        root.button(cx, ids!(fold_button)).set_visible(cx, !folded);
        root.button(cx, ids!(unfold_button)).set_visible(cx, folded);
        root.view(cx, ids!(fh_rule)).set_visible(cx, !folded);
    }

    /// A code row: the gutter's numbers and marks, the line's text and colours,
    /// and the caret when it is on this line.
    fn draw_code(&mut self, cx: &mut Cx2d, item: &WidgetRef, r: usize, pass: &DrawPass) {
        let Some(
            row @ Row::Code {
                kind, blob, line, ..
            },
        ) = pass.rows.get(r)
        else {
            return;
        };
        let d = pass.doc;
        // Text and spans are borrowed from the blob — never copied into the
        // row. See `concats_diff::blob`.
        let b = &d.blobs[*blob as usize];
        // A card ticked seen draws its marker down its whole left edge,
        // context lines included: the design puts the border on every line of
        // a seen card, and a marker only beside the changed lines read as a
        // dashed line rather than one border. Review state is still keyed per
        // changed line; this is just how a fully seen card renders.
        let seen = pass.review.seen.contains(&(b.oid, *line))
            || self
                .cards
                .containing(r)
                .is_some_and(|c| c.seen_state(&pass.review.seen) == Seen::All);
        // Inside the range being composed: marker + tint. Deleted rows check
        // the old side, everything else the new side. A reply selects no
        // lines — it holds its root's, which are already marked.
        let selected = matches!(d.compose, Some(Composing::Lines(c)) if {
            let side = match kind {
                LineKind::Del => c.old,
                _ => c.new,
            };
            side.is_some_and(|s| s.blob == *blob && *line >= s.start && *line <= s.end)
        });
        let marks = RowMarks {
            seen,
            // Inside a stored comment's range: the blue marker.
            commented: pass.review.commented.contains(&(b.oid, *line)),
            selected,
        };
        let text = b.line_text(*line as usize);
        // Coloured now if nothing has coloured it yet, so the first frame of a
        // file is never plain text.
        let drawn_spans = row_spans(b, *line as usize);
        let spans = match &drawn_spans {
            Some(spans) => spans.as_slice(),
            None => b.line_spans(*line as usize),
        };
        // The caret is in blob coordinates, so it lands on this row wherever
        // the row is drawn — and on every stream showing the same line.
        let caret = d
            .caret
            .filter(|c| c.blob == *blob && c.line == *line)
            .map(|c| c.byte);

        let (gut_ref, dl_ref) = (item.widget(cx, ids!(gut)), item.widget(cx, ids!(dl)));
        gut_ref.set_action_data(ReviewItemAction::Gutter {
            tab: pass.tab,
            row: r,
        });
        if let Some(mut g) = gut_ref.borrow_mut::<Gutter>() {
            g.set_row(row, marks);
        }
        if let Some(mut dl) = dl_ref.borrow_mut::<DiffLine>() {
            dl.set_row(*kind, text, spans, selected, caret);
            // Prose wraps whatever the setting says: a prompt is one line per
            // paragraph, and truncating it would hide what is being reviewed.
            dl.wrap = pass.wrap || b.prose.is_some();
            dl.hits = self
                .find
                .as_ref()
                .map(|find| find.hits(text))
                .unwrap_or_default();
            dl.quotes = d
                .quote_spans
                .get(&(*blob, *line))
                .map(|spans| {
                    spans
                        .iter()
                        .map(|(from, to)| (*from as usize, *to as usize))
                        .collect()
                })
                .unwrap_or_default();
            if pass.paint_selection {
                match pass
                    .selected
                    .and_then(|at| crate::review_doc::selection_on(at, r, text.len()))
                {
                    Some(bytes) => dl.selection_set(bytes.start, bytes.end),
                    None => dl.selection_clear(),
                }
            }
        }
        item.draw_all(cx, &mut Scope::with_props(&pass.theme));
        let band = item.area().rect(cx);
        self.drawn_rows.push(DrawnRow {
            row: r,
            top: band.pos.y,
            bottom: band.pos.y + band.size.y,
        });
        let (Some(mut g), Some(dl)) = (gut_ref.borrow_mut::<Gutter>(), dl_ref.borrow::<DiffLine>())
        else {
            return;
        };
        // The rail is drawn before the line it belongs to, so it only learns
        // how tall a soft-wrapped row is, and where the rows soft wrap added
        // start, once the line has been laid out.
        g.paint(
            cx,
            &mut Scope::with_props(&pass.theme),
            band.size.y,
            dl.rows.get(1..).unwrap_or_default(),
        );
        // Rows draw in order, so the last one carrying the selection is where
        // it ends — and that is where the chip goes.
        if let Some(rect) = dl.sel_rect {
            self.selection_end = Some((rect, r));
        }
        // Read back where the caret landed: the IME has to be told where
        // composed text will appear, and only the row that drew it knows.
        if caret.is_some() {
            self.caret_rect = dl.caret_rect;
        }
    }

    fn draw_composer(&mut self, cx: &mut Cx2d, item: &WidgetRef, pass: &DrawPass) {
        let input = item.text_input(cx, ids!(comp_input));
        let recreated = self.composer.widget_uid() != input.widget_uid();
        self.composer_input = Some(input.widget_uid());
        item.widget(cx, ids!(comp_post))
            .set_action_data(ReviewItemAction::Post);
        item.widget(cx, ids!(comp_cancel))
            .set_action_data(ReviewItemAction::Cancel);
        input.set_empty_text(cx, compose_title(pass.doc, &pass.review.comments));
        // NOTE: Virtualization can recreate the input; the window owns its draft.
        if input.text() != pass.draft {
            input.set_text(cx, pass.draft);
        }
        if recreated {
            input.set_selection(cx, self.composer.selection());
        }
        item.draw_all(cx, &mut Scope::with_props(&pass.theme));
        // NOTE: A new input has no focusable Area until it is drawn.
        if pass.focus_composer {
            input.set_key_focus(cx);
            if pass.requested_composer {
                self.state().with(|d| d.compose_focus = false);
            }
        }
        self.composer = input;
    }

    /// Park the chip at the end of the selection, when this list is the one
    /// holding it and nothing has been opened on it yet.
    ///
    /// `d.tab` is the stream whose gesture made the selection, so it is also
    /// the one that gets to offer the comment: every other list draws the same
    /// selection wherever it shows those lines, and three chips for one
    /// selection is three places to click for one comment.
    fn draw_chip(&mut self, cx: &mut Cx2d, pass: &DrawPass) {
        let chip = self.view.view(cx, ids!(quote_chip));
        let place = self
            .selection_end
            .filter(|_| pass.doc.tab == pass.tab && pass.doc.composer_tab != Some(pass.tab));
        let Some((rect, row)) = place else {
            if chip.visible() {
                chip.set_visible(cx, false);
            }
            return;
        };
        chip.set_visible(cx, true);
        self.view
            .widget(cx, ids!(quote_button))
            .set_action_data(ReviewItemAction::Quote {
                tab: pass.tab,
                near: row,
            });
        // Over the end of the selection rather than beside it: beside covers the
        // rest of the line, which is the one thing a reviewer is reading right
        // then. Under it instead when the selection ends at the top of the
        // list, where there is nothing above to sit in.
        let top = self.view.portal_list(cx, ids!(list)).area().rect(cx).pos.y;
        let above = rect.pos.y - CHIP_HEIGHT - CHIP_GAP;
        let y = if above >= top {
            above
        } else {
            rect.pos.y + rect.size.y + CHIP_GAP
        };
        if let Some(mut view) = chip.borrow_mut() {
            view.walk.abs_pos = Some(dvec2(rect.pos.x + rect.size.x - CHIP_GAP, y));
        }
    }

    /// Which card's header the pinned copy mirrors this pass, and where it
    /// sits: `(push, lift)` from [`sticky_offsets`].
    ///
    /// Item geometry comes from the list's scroll position and its
    /// measured-height tree, never from the drawn widgets' rects. A rect only
    /// exists for an item drawn this pass, and which items those are depends
    /// on scroll velocity, so the header being tracked would blink out of
    /// reach just when you scroll fast and the copy would pop instead of
    /// riding. This is defined for every entry, at any speed.
    // NOTE: picks the pinned header and places it in one look at the cards; split, it looks twice.
    // coland-ignore: @coland/read_write_operation
    fn place_sticky(
        &mut self,
        list: &PortalList,
        drawn: &[usize],
        range: &EntryRange,
        pass: &DrawPass,
    ) -> (f64, f64) {
        let item_rect = |entry: usize| Some((list.item_top(entry)?, list.item_height(entry)?));
        let bands: Vec<Band> = drawn
            .iter()
            .filter_map(|&entry| {
                let (top, height) = item_rect(entry)?;
                Some(Band { entry, top, height })
            })
            .collect();
        let first_entry = if range.remapped {
            range.first
        } else {
            range.old_first
        };
        let first = self
            .row_at(anchor_entry(&bands).unwrap_or(first_entry))
            .unwrap_or(0);
        let card = self.cards.containing(first);
        let header_top = card
            .and_then(|card| self.entry_of(card.header))
            .and_then(item_rect)
            .map(|(top, _)| top);

        // Present as long as the viewport top is inside the card, not only
        // once the header has climbed. While the real header is still on
        // screen the copy rides exactly on it (see `arrive`), so there is no
        // handover to see, and since the copy draws above the fade the card's
        // title is never masked by it. A folded card is the exception: it has
        // no rows to scroll past its header, so pinning one would leave a copy
        // hovering over the next card.
        self.sticky_idx = card
            .filter(|card| {
                !matches!(
                    pass.rows.get(card.header),
                    Some(Row::FileHeader { path, .. }) if pass.doc.folded.contains(path)
                )
            })
            .map(|card| card.header);

        let end_top = self
            .sticky_idx
            .and_then(|header| self.cards.at(header))
            .and_then(|card| self.entry_of(card.end))
            .and_then(item_rect)
            .map(|(top, _)| top);
        sticky_offsets(header_top, end_top)
    }

    /// The pinned header's widgets, written with the list no longer borrowed:
    /// a path lookup walks makepad's widget-tree cache, and refreshing a dirty
    /// node needs to borrow this widget's children — impossible while the draw
    /// loop holds the list, where the lookup silently returns an empty ref and
    /// every set on it is a no-op.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "The shader stores pixel dimensions as f32."
    )]
    fn draw_sticky(&mut self, cx: &mut Cx2d, pass: &DrawPass, push: f64, lift: f64) {
        let sticky = self.view.view(cx, ids!(sticky));
        let pinned = self
            .sticky_idx
            .filter(|&header| matches!(pass.rows.get(header), Some(Row::FileHeader { .. })));
        let Some(header) = pinned else {
            if sticky.visible() {
                sticky.set_visible(cx, false);
            }
            return;
        };
        self.fill_header(cx, &sticky, header, pass);
        if let Some(mut view) = sticky.borrow_mut() {
            // The up-shadow always takes the 12pt above the header inside the
            // sticky, so the box always starts that much higher for the header
            // to land where it belongs. Unconditional on purpose: as soon as
            // this offset depended on state, the header jumped 12pt the moment
            // the state flipped.
            view.walk.margin.top = push - STICKY_SHADOW;
        }
        // Ramp both shadows rather than toggling them.
        for ids in [ids!(st_shadow), ids!(st_shadow_up)] {
            if let Some(mut shadow) = self.view.widget(cx, ids).borrow_mut::<DropShadow>() {
                shadow.fade = lift as f32;
            }
        }
        if !sticky.visible() {
            sticky.set_visible(cx, true);
        }
    }

    /// Arm the platform text input at the caret. Without it, typed characters
    /// never arrive as `Event::TextInput` and the surface is read-only,
    /// however much key handling sits behind it.
    ///
    /// The IME is one thing for the whole app, not one per window, so only the
    /// focused window may touch it. Without that check every list in every
    /// other window calls `hide_text_ime` on each of its draws and tears down
    /// the IME the window with the caret just armed — which reads as the app
    /// going dead to the keyboard the moment a second window is open.
    fn arm_ime(&mut self, cx: &mut Cx2d) {
        if !self.state().is_focused() || !self.caret_focused(cx) {
            return;
        }
        match self.caret_rect {
            Some(rect) => {
                let area = self.view.portal_list(cx, ids!(list)).area();
                let origin = area.rect(cx).pos;
                cx.show_text_ime(area, rect.pos - origin);
            }
            None => cx.hide_text_ime(),
        }
    }
}

impl ReviewList {
    /// Which code row a window position falls on, from what this list actually
    /// drew. A position above or below every drawn row clamps to the nearest —
    /// dragging past the edge of the viewport extends to the edge rather than
    /// stopping dead.
    pub(super) fn row_at_y(&self, y: f64) -> Option<usize> {
        row_at_y(&self.drawn_rows, y)
    }
}

/// Which row a position falls on, over the bands a list drew. Pure so the
/// clamping is testable: this replaced arithmetic that turned a drag distance
/// into a row count, and that arithmetic was only ever right while every row
/// was the same height.
fn row_at_y(drawn: &[DrawnRow], y: f64) -> Option<usize> {
    if let Some(band) = drawn.iter().find(|band| y >= band.top && y < band.bottom) {
        return Some(band.row);
    }
    let first = drawn.first()?;
    let last = drawn.last()?;
    Some(if y < first.top { first.row } else { last.row })
}

#[cfg(test)]
mod tests {
    use super::{
        Band, CARD_END_EDGE, CardIndex, CardMeta, Cell, DrawnRow, FILE_HEADER_TOP_PADDING, HashSet,
        Row, STICKY_FADE, STICKY_HEIGHT, STICKY_TOP_GAP, Seen, anchor_entry, first_hunk,
        owns_gesture, row_at_y, sticky_offsets,
    };
    #[allow(
        clippy::wildcard_imports,
        reason = "Event fixtures are built from the widget prelude's types."
    )]
    use crate::makepad_widgets::{
        event::{TouchPoint, TouchState, TouchUpdateEvent},
        *,
    };

    fn press(x: f64) -> Event {
        Event::MouseDown(MouseDownEvent {
            abs: dvec2(x, 10.0),
            button: MouseButton::PRIMARY,
            window_id: WindowId(0, 0),
            modifiers: KeyModifiers::default(),
            handled: Cell::new(Area::Empty),
            time: 0.0,
        })
    }

    fn release() -> Event {
        Event::MouseUp(MouseUpEvent {
            abs: dvec2(0.0, 0.0),
            button: MouseButton::PRIMARY,
            window_id: WindowId(0, 0),
            modifiers: KeyModifiers::default(),
            time: 0.0,
        })
    }

    /// A release reaches every dock item, hidden tabs included, so a view
    /// answers for one only when its own press started it. Without this, a tab
    /// the reviewer visited once takes the caret — and the keyboard — off
    /// whatever they are using now.

    #[test]
    fn the_hunk_arrows_skip_hunks_the_view_does_not_show() {
        let hunk = || Row::HunkBar {
            old: None,
            new: None,
        };
        let rows = [
            Row::Spacer,
            hunk(),
            Row::Spacer,
            hunk(),
            hunk(),
            Row::Spacer,
        ];
        let every = |_| true;
        assert_eq!(first_hunk(&rows, 2..rows.len(), every), Some(3));
        assert_eq!(first_hunk(&rows, (0..3).rev(), every), Some(1));
        assert_eq!(first_hunk(&rows, 2..rows.len(), |row| row != 3), Some(4));
        assert_eq!(first_hunk(&rows, 5..rows.len(), every), None);
    }
    #[test]
    fn only_the_view_the_press_went_down_in_owns_the_gesture() {
        let rect = || Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(100.0, 100.0),
        };
        let mut pressed = false;
        // A release nobody pressed for is nobody's.
        assert!(!owns_gesture(&mut pressed, &release(), rect));

        // Pressed here: the release that follows is this view's, once.
        assert!(!owns_gesture(&mut pressed, &press(50.0), rect));
        assert!(owns_gesture(&mut pressed, &release(), rect));
        assert!(!owns_gesture(&mut pressed, &release(), rect));

        // Pressed somewhere else: every release after it belongs there.
        assert!(!owns_gesture(&mut pressed, &press(500.0), rect));
        assert!(!owns_gesture(&mut pressed, &release(), rect));
    }

    /// A touch drag updates the selection as it moves, so the gesture stays the
    /// pressed view's until the last finger lifts.
    #[test]
    fn a_touch_gesture_is_owned_from_its_start_until_it_lifts() {
        let rect = || Rect {
            pos: dvec2(0.0, 0.0),
            size: dvec2(100.0, 100.0),
        };
        let touch = |state: TouchState, x: f64| {
            Event::TouchUpdate(TouchUpdateEvent {
                time: 0.0,
                window_id: WindowId(0, 0),
                modifiers: KeyModifiers::default(),
                touches: vec![TouchPoint {
                    state,
                    abs: dvec2(x, 10.0),
                    time: 0.0,
                    uid: 0,
                    rotation_angle: 0.0,
                    force: 0.0,
                    radius: dvec2(0.0, 0.0),
                    handled: Cell::new(Area::Empty),
                    sweep_lock: Cell::new(Area::Empty),
                }],
            })
        };
        let mut pressed = false;
        assert!(owns_gesture(
            &mut pressed,
            &touch(TouchState::Start, 50.0),
            rect
        ));
        assert!(owns_gesture(
            &mut pressed,
            &touch(TouchState::Move, 50.0),
            rect
        ));
        assert!(owns_gesture(
            &mut pressed,
            &touch(TouchState::Stop, 50.0),
            rect
        ));
        assert!(!owns_gesture(
            &mut pressed,
            &touch(TouchState::Move, 50.0),
            rect
        ));

        assert!(!owns_gesture(
            &mut pressed,
            &touch(TouchState::Start, 500.0),
            rect
        ));
    }

    #[test]
    fn seen_summaries_only_visit_requested_cards_and_follow_invalidations() {
        let oid = gix::ObjectId::from_hex(&[b'1'; 40]).unwrap();
        let mut index = CardIndex {
            cards: (0..2)
                .map(|header| CardMeta {
                    header,
                    end: header,
                    keys: vec![(oid, 0), (oid, 1)],
                    seen: Cell::new(None),
                })
                .collect(),
        };
        let mut seen = HashSet::new();
        assert_eq!(index.at(0).unwrap().seen_state(&seen), Seen::Unseen);
        assert_eq!(index.at(1).unwrap().seen.get(), None);

        seen.insert((oid, 0));
        index.invalidate_seen();
        assert_eq!(index.at(0).unwrap().seen_state(&seen), Seen::Partly);
        assert_eq!(index.at(1).unwrap().seen.get(), None);

        seen.insert((oid, 1));
        index.invalidate_seen();
        assert_eq!(index.at(0).unwrap().seen_state(&seen), Seen::All);
        assert_eq!(index.at(1).unwrap().seen_state(&seen), Seen::All);
    }

    fn band(entry: usize, top: f64, height: f64) -> Band {
        Band { entry, top, height }
    }

    fn drawn(row: usize, top: f64, bottom: f64) -> DrawnRow {
        DrawnRow { row, top, bottom }
    }

    #[test]
    fn the_anchor_is_the_lowest_drawn_row_still_crossing_the_top_edge() {
        let bands = [
            band(3, -40.0, 20.0), // fully scrolled past — its band ends above the edge
            band(4, -10.0, 0.0),  // zero-height at the boundary: never crosses
            band(5, -10.0, 25.0), // crosses the edge — the anchor
            band(6, 15.0, 30.0),
        ];
        assert_eq!(anchor_entry(&bands), Some(5));
        assert_eq!(anchor_entry(&[]), None);
    }

    #[test]
    fn the_pinned_copy_rides_the_real_header_while_it_is_on_screen() {
        let (push, lift) = sticky_offsets(Some(100.0), None);
        assert_eq!(push, 100.0 + FILE_HEADER_TOP_PADDING);
        assert_eq!(lift, 0.0, "resting on the real header casts no shadow");
    }

    #[test]
    fn an_off_screen_header_rests_the_copy_at_the_gap_under_full_shadow() {
        assert_eq!(sticky_offsets(None, None), (STICKY_TOP_GAP, 1.0));
    }

    #[test]
    fn the_cards_bottom_edge_pushes_the_copy_out_instead_of_over_the_next_card() {
        // The card's end just above the top edge: the copy slides out with it…
        let end_top = STICKY_HEIGHT - CARD_END_EDGE - 20.0;
        assert_eq!(sticky_offsets(None, Some(end_top)).0, -20.0);
        // …but never past its own height, and never below its resting gap.
        assert_eq!(sticky_offsets(None, Some(-1000.0)).0, -STICKY_HEIGHT);
        assert_eq!(sticky_offsets(None, Some(1000.0)).0, STICKY_TOP_GAP);
    }

    #[test]
    fn the_shadow_ramps_continuously_as_the_header_lifts_away() {
        let lift_at = |top: f64| sticky_offsets(Some(top), None).1;
        let start = STICKY_TOP_GAP - FILE_HEADER_TOP_PADDING;
        assert_eq!(lift_at(start), 0.0);
        assert_eq!(lift_at(start - STICKY_FADE / 2.0), 0.5);
        assert_eq!(lift_at(start - STICKY_FADE), 1.0);
    }

    #[test]
    fn a_position_names_the_row_whose_band_it_lands_in() {
        let rows = [
            drawn(4, 100.0, 120.0),
            drawn(5, 120.0, 160.0),
            drawn(6, 160.0, 180.0),
        ];
        assert_eq!(row_at_y(&rows, 100.0), Some(4));
        assert_eq!(row_at_y(&rows, 119.9), Some(4));
        assert_eq!(row_at_y(&rows, 120.0), Some(5), "bands are half-open");
        assert_eq!(row_at_y(&rows, 159.0), Some(5), "the tall row is one row");
        assert_eq!(row_at_y(&rows, 170.0), Some(6));
    }

    /// Dragging past the viewport extends to the edge rather than stopping.
    #[test]
    fn a_position_beyond_what_was_drawn_clamps_to_the_nearest_row() {
        let rows = [drawn(4, 100.0, 120.0), drawn(5, 120.0, 140.0)];
        assert_eq!(row_at_y(&rows, 0.0), Some(4));
        assert_eq!(row_at_y(&rows, 9999.0), Some(5));
        assert_eq!(row_at_y(&[], 50.0), None);
    }
}
