//! The review list: one flat, virtualized `PortalList` with a template per row
//! kind (prose, file-card chrome, code rows, comments, the inline composer).
//! Every dock tab owns one instance, pinned to a stream by its `kind`. The code
//! rows compose the `Gutter` + `DiffLine` pair; a sticky overlay mirrors the
//! card header that has scrolled past the top so its path and tick box stay in
//! reach. Both headers emit the same actions, addressed by file path.

use std::{collections::HashSet, sync::Arc};

use concats_diff::{Blob, LineKind, Row};
use concats_review::store::{self, LineKey};

use super::{
    DiffLine, Gutter,
    collapsed_run::CollapsedRun,
    drop_shadow::{DropShadow, ShadowUp},
    gutter::RowMarks,
};
use crate::{
    FrameData, FrameTheme, editor,
    file_view::{relower_edited, save_plan},
    makepad_widgets::*,
    review_doc::{Composing, FileView, ReviewDoc, Stream, compose_title},
    service::{HighlightCmd, ReviewCmd, ReviewState, highlight, review},
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
/// on whether the worker had finished, and the two split a line differently
/// (this one merges adjacent runs of one colour, the worker keeps a run per
/// capture), so the same file rendered two slightly different ways depending on
/// timing. One source, no race, no flash.
///
/// It is the only source for a buffer with unsaved edits too, and there it is
/// the only correct one: the worker keys its trees by oid, typing does not
/// change the oid, so its answer would colour the text from before the edit.
#[cfg(feature = "treesitter")]
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

#[cfg(not(feature = "treesitter"))]
fn draws_own_spans(_blob: &Blob) -> bool {
    false
}

/// This row's colours, when the draw is the one colouring it.
fn row_spans(blob: &Blob, line: usize) -> Option<Vec<concats_syntax::Span>> {
    #[cfg(feature = "treesitter")]
    if draws_own_spans(blob) {
        return Some(DRAW_HIGHLIGHTER.with_borrow_mut(|hl| {
            hl.spans_for_line(
                concats_highlight::Buffer {
                    oid: blob.oid,
                    rev: blob.edit_rev,
                    editable: blob.editable(),
                    ext: &blob.ext,
                    text: &blob.text,
                },
                line,
            )
        }));
    }
    let _ = (blob, line);
    None
}

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
fn sticky_offsets(header_top: Option<f64>, end_top: Option<f64>) -> (f64, f64) {
    // Where the real header sits right now, unclamped.
    let natural = header_top.map(|top| top + FILE_HEADER_TOP_PADDING);
    // Riding up: sit on the real header until it reaches the rest.
    let arrive = natural.unwrap_or(STICKY_TOP_GAP).max(STICKY_TOP_GAP);
    // Leaving: once the card's bottom edge reaches the top, the copy slides out
    // with it instead of covering the next card — the handoff CSS `position:
    // sticky` gives you for free.
    let leave = end_top
        .map(|top| (top + CARD_END_EDGE - STICKY_HEIGHT).clamp(-STICKY_HEIGHT, STICKY_TOP_GAP))
        .unwrap_or(STICKY_TOP_GAP);
    let push = if arrive > STICKY_TOP_GAP {
        arrive
    } else {
        leave
    };
    // How far the header floats above its resting place, 0..1 — the shadow's
    // opacity. It is continuous, so there is no scroll position where the
    // shadow suddenly appears.
    let lift = natural
        .map(|top| ((STICKY_TOP_GAP - top) / STICKY_FADE).clamp(0.0, 1.0))
        .unwrap_or(1.0);
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
    },
    Delete {
        id: u64,
    },
    Reply {
        tab: Stream,
        id: u64,
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
    use mod.widgets.C_BG
    use mod.widgets.C_CARD
    use mod.widgets.C_BORDER
    use mod.widgets.C_TEXT
    use mod.widgets.C_DIM
    use mod.widgets.C_FAINT
    use mod.widgets.C_YELLOW
    use mod.widgets.C_ACCENT
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
        draw_bg +: {
            border_size: 0.0
            color: mod.app_theme.color_bg
            color_hover: mod.app_theme.color_bg
            color_focus: mod.app_theme.color_bg
            color_down: mod.app_theme.color_bg
            color_empty: mod.app_theme.color_bg
            color_disabled: mod.app_theme.color_bg
        }
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

    // A composer control: 20x20 around a 12px glyph, like the status bar's.
    let IconAction = ButtonFlatter {
        width: 20 height: 20
        padding: 0
        margin: 0
        text: ""
        icon_walk: Walk{width: 12 height: Fit}
        draw_icon +: {
            color: C_DIM
            color_hover: C_TEXT
        }
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

    // One code row's content: the gutter rail and the line. Shared by the
    // card-framed `Code` and the bare `CodeFlat`, so the two never drift.
    let CodeRow = SolidView {
        width: Fill height: Fit
        draw_bg.color: C_CARD
        flow: Right
        gut := Gutter {}
        dl := DiffLine {}
    }

    // A comment strip, shared by `Comment` and its indented `Reply` twin.
    // Hoisted so the two differ in one line — see the entries in the list.
    // Every level is named, so `Reply` can reach the innermost box: a `+:`
    // merge resolves a direct child, and each level in between has to be
    // addressable for the nested form to get there.
    let CommentStrip = View {
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
                    flow: Right
                    align: Align{x: 0.0, y: 0.0}
                    padding: Inset{top: 12 bottom: 12 left: 12 right: 12}
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
                        cm_body := Prose {
                            width: Fill
                            draw_text.color: C_TEXT
                            draw_text.text_style: FONT{font_size: 9}
                        }
                    }
                    cm_reply := IconAction {
                        draw_icon.svg: crate_resource("self:resources/icons/reply.svg")
                    }
                    cm_delete := IconAction {
                        draw_icon.svg: crate_resource("self:resources/icons/close_circle.svg")
                    }
                }
            }
        }
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

    mod.widgets.ReviewList = #(ReviewList::register_widget(vm)) {
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

            // The top of a file card: fold caret, viewed tick box, path,
            // ±stats. The nested views fake a 1px border with rounded top
            // corners; the code rows below continue the side borders and
            // CardEnd closes the bottom — a "card" over a flat virtualized
            // list. The title row is a fixed 32, like the design: a Fit height
            // paints short of the box it lays out and left a band of window
            // background between the header and the first code row. Fixed also
            // because the body is hidden while the pinned copy stands in for
            // it, and a Fit box would collapse to nothing — the row leaves the
            // list, everything shunts up, the scroll resets. 16 lead-in + 1
            // border + 30 row + 1 rule.
            FileHeader := View {
                width: Fill height: 48
                padding: Inset{top: 16 bottom: 0 left: 16 right: 16}
                flow: Overlay
                fh_body := View {
                    width: Fill height: Fit
                    flow: Overlay
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BORDER
                    padding: Inset{top: 1 left: 1 right: 1}
                    SolidView {
                        width: Fill height: Fit
                        draw_bg.color: C_CARD
                        flow: Down

                        View {
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
                                // A right caret's ink is 1:2, and the svg
                                // scales from its ink bounds, not the viewbox:
                                // ask for 6 wide so Fit lands on 12 tall.
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
                            // Conversations this range cannot place. Shown
                            // only when there are some, because a card with
                            // nothing hidden has nothing to offer.
                            fh_outdated := OutdatedToggle {}
                            fh_stat := Label {
                                width: Fit
                                draw_text.color: C_FAINT
                                draw_text.text_style: FONT{font_size: 9}
                            }
                        }
                        // The hairline under the title, dropped while the card
                        // is folded (the CardEnd cap closes it instead).
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
                }
            }

            // A collapsed run of unchanged lines: the arrow band(s) that reveal
            // it. Wrapped like a code row so the card's side borders carry
            // straight through the cut.
            Skipped := View {
                width: Fill height: Fit
                padding: Inset{left: 16 right: 16}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BORDER
                    padding: Inset{left: 1 right: 1}
                    collapsed := CollapsedRun {}
                }
            }

            // What the range removed, where it was removed from. The file view
            // shows the file as it is, so this stands in for lines the head
            // does not have — press it and they take its place.
            Removed := View {
                width: Fill height: Fit
                padding: Inset{left: 16 right: 16}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BORDER
                    padding: Inset{left: 1 right: 1}
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
            Spacer := View {
                width: Fill height: Fit
                padding: Inset{left: 16 right: 16}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BORDER
                    padding: Inset{left: 1 right: 1}
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
            Code := View {
                width: Fill height: Fit
                padding: Inset{left: 16 right: 16}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BORDER
                    padding: Inset{left: 1 right: 1}
                    CodeRow {}
                }
            }
            // The same row in a File tab, which is an editor and not a code
            // block: no card sides, no inset, the text against the pane.
            CodeFlat := CodeRow {}

            // The inline comment composer, spliced below the last selected line
            // — the same strip a posted comment gets, so the blue bar ties it
            // to the selected range the same way. One box and two icons, like
            // the design: no well, no title, the placeholder names the target
            // range, send and dismiss are the icons.
            Composer := View {
                width: Fill height: Fit
                padding: Inset{left: 16 right: 16}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BORDER
                    padding: Inset{top: 1 bottom: 1 left: 1 right: 1}
                    SolidView {
                        width: Fill height: Fit
                        draw_bg.color: C_ACCENT
                        padding: Inset{left: 6}
                        SolidView {
                            width: Fill height: Fit
                            draw_bg.color: C_BG
                            flow: Down
                            spacing: 10
                            padding: Inset{top: 12 bottom: 12 left: 12 right: 12}

                            comp_input := FlatInput {
                                width: Fill
                                empty_text: "Add a comment"
                            }
                            View {
                                width: Fill height: Fit
                                flow: Right
                                spacing: 1
                                align: Align{x: 1.0, y: 0.5}
                                comp_cancel := IconAction {
                                    draw_icon.svg: crate_resource("self:resources/icons/close_circle.svg")
                                }
                                SolidView {
                                    width: 1 height: 16
                                    margin: Inset{left: 4 right: 4}
                                    draw_bg.color: C_BORDER
                                }
                                comp_post := IconAction {
                                    draw_icon.svg: crate_resource("self:resources/icons/send.svg")
                                }
                            }
                        }
                    }
                }
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

        // The find bar. Pinned over the list rather than spliced into it: it
        // belongs to the view, not to the document, and a row that scrolled it
        // out of reach would be a search box you cannot see while searching.
        find_bar := View {
            visible: false
            width: Fill height: Fit
            align: Align{x: 1.0, y: 0.0}
            padding: Inset{top: 8 right: 24}
            SolidView {
                width: 420 height: Fit
                draw_bg.color: C_BORDER
                padding: Inset{top: 1 bottom: 1 left: 1 right: 1}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_BG
                    flow: Down
                    padding: Inset{top: 8 bottom: 8 left: 10 right: 10}
                    spacing: 6
                    View {
                        width: Fill height: Fit
                        flow: Right
                        align: Align{y: 0.5}
                        spacing: 8
                        find_input := FlatInput {
                            width: Fill height: 20
                            is_multiline: false
                            submit_on_enter: true
                            empty_text: "Find"
                        }
                        find_count := Prose {
                            width: Fit height: Fit
                            draw_text.color: C_FAINT
                            draw_text.text_style: FONT{font_size: 8.5}
                        }
                    }
                    replace_input := FlatInput {
                        width: Fill height: 20
                        is_multiline: false
                        submit_on_enter: true
                        empty_text: "Replace (enter replaces, cmd-enter all)"
                    }
                }
            }
        }

        // The tab strip's shadow over the content. Drawn before the pinned
        // header so the header sits in front of it, the way it does in the
        // design: the strip's shadow is clipped short by the header's own top.
        DropShadow { height: 16 }

        // The pinned copy of the card header whose rows fill the top of the
        // viewport: the header scrolls along, so the path and the viewed tick
        // box stay at hand until the card is read (and ticked) to its end.
        // Populated and shown by ReviewList::draw_walk.
        //
        // Chrome-identical to a FileHeader's card box (its 16px lead-in aside):
        // same border, same hairline, same rounded top corners. The copy rides
        // on the real header while it climbs, so any difference between the two
        // reads as the header changing shape mid-scroll.
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
            st_shadow_up := ShadowUp {
                width: Fill height: 12
            }
            View {
            width: Fill height: Fit
            flow: Overlay
            SolidView {
                width: Fill height: Fit
                draw_bg.color: C_BORDER
                padding: Inset{top: 1 left: 1 right: 1}
                SolidView {
                    width: Fill height: Fit
                    draw_bg.color: C_CARD
                    flow: Down
                    View {
                        width: Fill height: 30
                        flow: Right
                        spacing: 8
                        align: Align{x: 0.0, y: 0.5}
                        padding: Inset{left: 8 right: 8}
                        st_fold := FoldCaret {
                            draw_icon.svg: crate_resource("self:resources/icons/caret_down.svg")
                        }
                        st_seen := SeenBox {}
                        st_path := Label {
                            width: Fit
                            draw_text.color: C_TEXT
                            draw_text.text_style: FONT{font_size: 9}
                        }
                        View { width: Fill height: Fit }
                        // The pinned twin of the card header's toggle: the
                        // in-list header is invisible while this stands in for
                        // it, so a control that only lived there would be out
                        // of reach just when the card is on screen.
                        st_outdated := OutdatedToggle {}
                        st_stat := Label {
                            width: Fit
                            draw_text.color: C_FAINT
                            draw_text.text_style: FONT{font_size: 9}
                        }
                    }
                    st_rule := SolidView {
                        width: Fill height: 1
                        draw_bg.color: C_BORDER
                    }
                }
            }
            View {
                width: Fill height: Fill
                align: Align{x: 0.0, y: 0.0}
                CardCap {}
            }
            // Pinned it is a free-floating box, so it rounds at the bottom too.
            View {
                width: Fill height: Fill
                align: Align{x: 0.0, y: 1.0}
                CardCapBottom {}
            }
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

#[derive(Script, ScriptHook, Widget)]
pub struct ReviewList {
    #[deref]
    view: View,
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
    /// on it is a no-op — so its keystrokes arrive unlabelled and ReviewPane
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
    highlight_generation: u64,
    #[rust]
    requested_highlights: HashSet<(u32, u64)>,
    #[rust]
    mapped_generation: u64,
    #[rust]
    mapped_comments_rev: u64,
    /// The row-stream shape this instance's caches were built against. Splicing
    /// the composer inserts a row mid-stream, which shifts every row index after
    /// it — so `cards` (card boundaries by row) and `visible` (entry→row) are
    /// stale, and every card below the composer resolved to the wrong geometry:
    /// its header stopped sticking.
    #[rust]
    mapped_rows_rev: u64,
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
    find: Option<String>,
    #[rust]
    find_at: Option<(usize, usize)>,
    #[rust]
    seek_row: Option<usize>,
    /// The stream this instance last drew. `tab_of` walks the widget tree,
    /// which only resolves while drawing; asked during event handling, every
    /// instance fell back to the same answer, and gating a keystroke on that
    /// applied it once per instance. So it is captured here.
    #[rust]
    drawn_tab: Option<Stream>,
}

#[derive(Default)]
struct CardIndex {
    cards: Vec<CardMeta>,
}

struct CardMeta {
    header: usize,
    end: usize,
    keys: Vec<LineKey>,
    all_seen: bool,
    any_seen: bool,
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
                    all_seen: false,
                    any_seen: false,
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
                _ => {}
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

    fn update_seen(&mut self, seen: &HashSet<LineKey>) {
        for card in &mut self.cards {
            card.all_seen = !card.keys.is_empty() && card.keys.iter().all(|key| seen.contains(key));
            card.any_seen = card.keys.iter().any(|key| seen.contains(key));
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

    /// Rebuild the entry→row mapping for this stream's fold state. A folded
    /// card keeps its header and its bottom cap; everything between them —
    /// hunks, comments, the composer — stops being an entry at all, so the
    /// list never walks (or measures) the rows it isn't drawing.
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

/// Whether the rows leading away from a collapsed run reach code before they
/// reach the card's chrome — that decides whether the end gets an expander. It
/// takes the stream in either direction, so one walk answers both sides. The
/// rows it steps over draw nothing between the run and the code: the hunk
/// anchor has no height, and the card's 10pt of air is the padding around the
/// code itself.
fn reaches_code<'a>(mut rows: impl Iterator<Item = &'a Row>) -> bool {
    rows.find(|row| !matches!(row, Row::HunkBar { .. } | Row::Spacer))
        .is_some_and(|row| matches!(row, Row::Code { .. } | Row::Comment { .. } | Row::Composer))
}

/// The card header's title: a rename shows as "old → new", not as two files.
fn header_title(path: &str, from: Option<&str>) -> String {
    match from {
        Some(f) => format!("{f}  →  {path}"),
        None => path.to_string(),
    }
}

/// The card header's right-hand stat, shared with the sticky copy.
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
            && (self.mapped_generation != frame.document.generation
                || self.mapped_rows_rev != frame.document.rows_rev)
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
            wrap: crate::theme::active_font().wrap,
            window: frame.state.id,
        };
        let range = self.refresh_caches(&pass, list_ref.first_id());
        // Re-established by whichever row draws the caret this pass; stale
        // geometry would aim the IME at a line that has scrolled away.
        self.caret_rect = None;
        self.drawn_rows.clear();
        let find_bar = self.view.view(cx, ids!(find_bar));
        if find_bar.visible() != self.find.is_some() {
            find_bar.set_visible(cx, self.find.is_some());
        }

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
                if pass.requested_composer
                    && let Some(row) = pass
                        .rows
                        .iter()
                        .position(|row| matches!(row, Row::Composer))
                    && let Some(entry) = if self.visible.is_empty() {
                        Some(row)
                    } else {
                        self.visible.binary_search(&row).ok()
                    }
                {
                    list.set_first_id_and_scroll(entry, 0.0);
                }
                if let Some(row) = self.seek_row.take()
                    && let Some(entry) = if self.visible.is_empty() {
                        Some(row)
                    } else {
                        self.visible.binary_search(&row).ok()
                    }
                {
                    list.set_first_id_and_scroll(entry, 0.0);
                }
                let mut drawn = Vec::new();
                while let Some(entry) = list.next_visible_item(cx) {
                    drawn.push(entry);
                    self.draw_row(cx, &mut list, entry, &pass);
                }
                self.place_sticky(&list, &drawn, &range, &pass)
            };
            self.draw_sticky(cx, &pass, push, lift);
        }
        if std::mem::take(&mut self.focus_find) {
            let input = self.view.text_input(cx, ids!(find_input));
            input.set_text(cx, self.find.as_deref().unwrap_or_default());
            input.set_key_focus(cx);
        }
        self.arm_ime(cx);
        DrawStep::done()
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
        self.view.handle_event(cx, event, scope);
        // ...and its position is read back afterwards, because the gesture that
        // moves it is the list's own drag. The list holds it in item indices,
        // which only name rows currently on screen; the document holds it in
        // blob coordinates, which outlive scrolling and resplicing.
        if matches!(event, Event::MouseUp(_) | Event::TouchUpdate(_)) {
            self.adopt_list_selection(cx);
        }
        if let Event::Actions(actions) = event {
            self.handle_find(cx, actions);
        }
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
    fn refresh_caches(&mut self, pass: &DrawPass, old_first_id: usize) -> EntryRange {
        let d = pass.doc;
        if self.highlight_generation != d.generation {
            self.highlight_generation = d.generation;
            self.requested_highlights.clear();
        }
        let document_changed = self.mapped_generation != d.generation;
        let comments_changed = self.mapped_comments_rev != pass.review.comments_rev;
        // The composer moving is a change of the same kind as a comment landing:
        // a row appears or leaves mid-stream, so everything keyed by row index
        // has to be rebuilt.
        let rows_changed = self.mapped_rows_rev != d.rows_rev;
        let mapping_changed =
            document_changed || comments_changed || rows_changed || self.mapped_folded != d.folded;
        let previous = self.mapped_entries;
        let old_first_row = self.row_at(old_first_id).unwrap_or(0);
        if mapping_changed {
            self.map_folded(pass.rows, &d.folded);
            self.mapped_generation = d.generation;
            self.mapped_comments_rev = pass.review.comments_rev;
            self.mapped_rows_rev = d.rows_rev;
            self.mapped_folded.clone_from(&d.folded);
        }
        if document_changed || comments_changed || rows_changed {
            self.cards.rebuild(pass.rows, &d.blobs);
        }
        if document_changed
            || comments_changed
            || rows_changed
            || self
                .mapped_seen
                .as_ref()
                .is_none_or(|seen| !Arc::ptr_eq(seen, &pass.review.seen))
        {
            self.cards.update_seen(&pass.review.seen);
            self.mapped_seen = Some(pass.review.seen.clone());
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
                self.draw_file_header(cx, &item, r, pass);
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
                    s.set_ends(
                        reaches_code(pass.rows[..r].iter().rev()),
                        reaches_code(pass.rows[r + 1..].iter()),
                    );
                }
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            // Air, but part of the code region: it carries the seen marker,
            // which stops at a collapsed run.
            Row::Spacer => {
                let item = list.item(cx, entry, id!(Spacer));
                let seen = self.cards.containing(r).is_some_and(|card| card.all_seen);
                item.view(cx, ids!(sp_mark)).set_visible(cx, seen);
                item.draw_all(cx, &mut Scope::with_props(theme));
            }
            Row::Code { blob, .. } => {
                self.request_highlight(*blob, pass);
                // A File tab is an editor: its rows carry no card.
                let template = if self.kind == id!(file) {
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
            highlight().send(HighlightCmd::Request {
                window: pass.window,
                doc: pass.doc.clone(),
                generation: pass.doc.generation,
                blob,
                rev: b.edit_rev,
            });
        }
    }

    fn draw_file_header(&self, cx: &mut Cx2d, item: &WidgetRef, r: usize, pass: &DrawPass) {
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
        let (d, tab) = (pass.doc, pass.tab);
        // Invisible while the pinned copy stands in for this card — and inert
        // with it, since an invisible View drops the events its caret and tick
        // box need. The row keeps its height, so nothing moves.
        item.view(cx, ids!(fh_body))
            .set_visible(cx, self.sticky_idx != Some(r));
        item.widget(cx, ids!(seen_box))
            .set_action_data(ReviewItemAction::Seen {
                tab,
                path: path.clone(),
                near: r,
            });
        for ids in [ids!(fold_button), ids!(unfold_button)] {
            item.widget(cx, ids)
                .set_action_data(ReviewItemAction::Fold { path: path.clone() });
        }
        item.label(cx, ids!(fh_path))
            .set_text(cx, &header_title(path, from.as_deref()));
        // The viewed tick box covers every hunk of this card — all their
        // changed lines flip together.
        let (all, any) = self
            .cards
            .at(r)
            .map(|card| (card.all_seen, card.any_seen))
            .unwrap_or_default();
        item.check_box(cx, ids!(seen_box))
            .set_active(cx, all, Animate::No);
        item.label(cx, ids!(fh_stat))
            .set_text(cx, &header_stat(*adds, *dels, *similarity, any && !all));
        // Conversations recorded against this file that this range cannot
        // place. Offered only when there are some; the label says which way
        // the click goes. What the last comment splice managed to place is
        // read, not recomputed: the splice decided, and deciding again here is
        // how a header ends up disagreeing with the rows right under it.
        let hidden = store::outdated_threads(&pass.review.comments, path, &d.placed_threads);
        let outdated = item.button(cx, ids!(fh_outdated));
        outdated.set_visible(cx, hidden > 0);
        if hidden > 0 {
            outdated.set_text(
                cx,
                &outdated_label(hidden, d.show_all_comments.contains(path)),
            );
            item.widget(cx, ids!(fh_outdated))
                .set_action_data(ReviewItemAction::Outdated { path: path.clone() });
        }
        // Shut cards show the other caret and drop the hairline: the bottom
        // cap closes them instead.
        let folded = d.folded.contains(path);
        item.button(cx, ids!(fold_button)).set_visible(cx, !folded);
        item.button(cx, ids!(unfold_button)).set_visible(cx, folded);
        item.view(cx, ids!(fh_rule)).set_visible(cx, !folded);
        item.draw_all(cx, &mut Scope::with_props(&pass.theme));
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
            || self.cards.containing(r).is_some_and(|c| c.all_seen);
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
            .map(|c| c.byte as usize);

        item.widget(cx, ids!(gut))
            .set_action_data(ReviewItemAction::Gutter {
                tab: pass.tab,
                row: r,
            });
        if let Some(mut g) = item.widget(cx, ids!(gut)).borrow_mut::<Gutter>() {
            g.set_row(row, marks);
        }
        if let Some(mut dl) = item.widget(cx, ids!(dl)).borrow_mut::<DiffLine>() {
            dl.set_row(*kind, text, spans, selected, caret);
            dl.wrap = pass.wrap;
            dl.set_hits(hits_in(text, self.find.as_deref()));
            if pass.paint_selection {
                match crate::review_doc::selection_on(d, *blob, *line) {
                    Some((from, to)) => dl.selection_set(from, to),
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
        // Read back where the caret landed: the IME has to be told where
        // composed text will appear, and only the row that drew it knows.
        if caret.is_some() {
            self.caret_rect = item
                .widget(cx, ids!(dl))
                .borrow::<DiffLine>()
                .and_then(|dl| dl.caret_rect());
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

    /// Which card's header the pinned copy mirrors this pass, and where it
    /// sits: `(push, lift)` from [`sticky_offsets`].
    ///
    /// Item geometry comes from the list's scroll position and its
    /// measured-height tree, never from the drawn widgets' rects. A rect only
    /// exists for an item drawn this pass, and which items those are depends
    /// on scroll velocity, so the header being tracked would blink out of
    /// reach just when you scroll fast and the copy would pop instead of
    /// riding. This is defined for every entry, at any speed.
    fn place_sticky(
        &mut self,
        list: &PortalList,
        drawn: &[usize],
        range: &EntryRange,
        pass: &DrawPass,
    ) -> (f64, f64) {
        let item_rect = |entry: usize| Some((list.item_top(entry)?, list.item_height(entry)?));
        let entry_of = |row: usize| {
            if self.visible.is_empty() {
                Some(row)
            } else {
                self.visible.binary_search(&row).ok()
            }
        };
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
            .and_then(|card| entry_of(card.header))
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
            .and_then(|card| entry_of(card.end))
            .and_then(item_rect)
            .map(|(top, _)| top);
        sticky_offsets(header_top, end_top)
    }

    /// The pinned header's widgets, written with the list no longer borrowed:
    /// a path lookup walks makepad's widget-tree cache, and refreshing a dirty
    /// node needs to borrow this widget's children — impossible while the draw
    /// loop holds the list, where the lookup silently returns an empty ref and
    /// every set on it is a no-op.
    fn draw_sticky(&mut self, cx: &mut Cx2d, pass: &DrawPass, push: f64, lift: f64) {
        let sticky = self.view.view(cx, ids!(sticky));
        let pinned = self
            .sticky_idx
            .and_then(|header| Some((header, pass.rows.get(header)?)));
        let Some((
            header,
            Row::FileHeader {
                path,
                adds,
                dels,
                from,
                similarity,
                ..
            },
        )) = pinned
        else {
            if sticky.visible() {
                sticky.set_visible(cx, false);
            }
            return;
        };
        self.view
            .widget(cx, ids!(st_seen))
            .set_action_data(ReviewItemAction::Seen {
                tab: pass.tab,
                path: path.clone(),
                near: header,
            });
        self.view
            .widget(cx, ids!(st_fold))
            .set_action_data(ReviewItemAction::Fold { path: path.clone() });
        self.view
            .widget(cx, ids!(st_outdated))
            .set_action_data(ReviewItemAction::Outdated { path: path.clone() });
        self.view
            .label(cx, ids!(st_path))
            .set_text(cx, &header_title(path, from.as_deref()));
        let (all, any) = self
            .cards
            .at(header)
            .map(|card| (card.all_seen, card.any_seen))
            .unwrap_or_default();
        self.view
            .check_box(cx, ids!(st_seen))
            .set_active(cx, all, Animate::No);
        self.view
            .label(cx, ids!(st_stat))
            .set_text(cx, &header_stat(*adds, *dels, *similarity, any && !all));
        let hidden = store::outdated_threads(&pass.review.comments, path, &pass.doc.placed_threads);
        let st_outdated = self.view.button(cx, ids!(st_outdated));
        st_outdated.set_visible(cx, hidden > 0);
        if hidden > 0 {
            st_outdated.set_text(
                cx,
                &outdated_label(hidden, pass.doc.show_all_comments.contains(path)),
            );
        }
        if let Some(mut view) = sticky.borrow_mut() {
            // The up-shadow always takes the 12pt above the header inside the
            // sticky, so the box always starts that much higher for the header
            // to land where it belongs. Unconditional on purpose: as soon as
            // this offset depended on state, the header jumped 12pt the moment
            // the state flipped.
            view.walk.margin.top = push - STICKY_SHADOW;
        }
        // Ramp both shadows rather than toggling them.
        if let Some(mut shadow) = self
            .view
            .widget(cx, ids!(st_shadow))
            .borrow_mut::<DropShadow>()
        {
            shadow.fade = lift as f32;
        }
        if let Some(mut shadow) = self
            .view
            .widget(cx, ids!(st_shadow_up))
            .borrow_mut::<ShadowUp>()
        {
            shadow.fade = lift as f32;
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
    /// The find bar's two fields: the query as it is typed, Enter to walk the
    /// matches, and Enter in the second field to replace.
    fn handle_find(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.find.is_none() {
            return;
        }
        let find = self.view.text_input(cx, ids!(find_input));
        // Every keystroke: the hits are recomputed per row per draw, so simply
        // redrawing is what makes them follow what you have typed so far.
        if let Some(query) = find.changed(actions) {
            self.find = Some(query);
            self.find_at = None;
            self.count_hits(cx);
            self.redraw(cx);
        }
        if let Some((_, modifiers)) = find.returned(actions) {
            self.seek(cx, modifiers.shift);
            find.set_key_focus(cx);
        }
        let replace = self.view.text_input(cx, ids!(replace_input));
        if let Some((with, modifiers)) = replace.returned(actions) {
            self.replace(cx, &with, modifiers.logo || modifiers.control);
            self.count_hits(cx);
            replace.set_key_focus(cx);
        }
    }
}

impl ReviewList {
    /// The bar's tally over the stream this tab renders.
    fn count_hits(&mut self, cx: &mut Cx) {
        let query = self.find.clone().unwrap_or_default();
        let count = self.drawn_tab.map_or(0, |tab| {
            self.state()
                .read(|d| editor::search_hits(d, tab, &query).len())
        });
        let label = match (count, query.is_empty()) {
            (_, true) => String::new(),
            (0, _) => "no matches".into(),
            (1, _) => "1 match".into(),
            (n, _) => format!("{n} matches"),
        };
        self.view.label(cx, ids!(find_count)).set_text(cx, &label);
    }
}

impl ReviewList {
    /// Move the caret to wherever the list's pointer gesture just landed.
    ///
    /// NOTE: A drag supplies both endpoints so typing can replace the selection.
    fn adopt_list_selection(&mut self, cx: &mut Cx) {
        let list = self.view.portal_list(cx, ids!(list));
        let Some((start, end)) = list.borrow().and_then(|l| l.get_selection_range()) else {
            return;
        };
        let row = self.row_at(end.0);
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let head_row = row;
        let tail_row = self.row_at(start.0);
        let mut focus = false;
        self.state().with(|d| {
            let head = head_row.and_then(|row| editor::caret_at(d, tab, row, end.1));
            let tail = (start != end)
                .then(|| tail_row.and_then(|row| editor::caret_at(d, tab, row, start.1)))
                .flatten();
            if head.is_some() {
                // This stream owns the caret now, which is what routes
                // keystrokes to one list rather than to every instance.
                d.tab = tab;
                focus = true;
            }
            // NOTE: Editing needs both endpoints to replace the selected text.
            d.selection_anchor = tail;
            d.caret = head;
        });
        // Claim key focus for the surface the caret is on. Without it nothing
        // holds focus after a press on a code row (`DiffLine` is not
        // interactive on purpose) and typed characters have no owner.
        if focus {
            let area = self.view.portal_list(cx, ids!(list)).area();
            cx.set_key_focus(area);
        }
        self.redraw(cx);
    }

    /// Typed text — from the keyboard, an IME, or a paste. Returns whether the
    /// caret took it.
    fn caret_text(&mut self, cx: &mut Cx, input: &str) -> bool {
        if !self.caret_focused(cx) {
            return false;
        }
        let took = self.state().with(|d| editor::type_text(d, input));
        if took {
            self.after_edit(cx);
        }
        took
    }

    /// Whether this list is the one a keystroke should reach.
    ///
    /// Key focus alone cannot decide it: several lists of the same kind exist
    /// at once (the dock keeps an instance per pane it can show), and they all
    /// answer `has_key_focus` for the same area, so gating on focus alone
    /// applied every keystroke once per instance. The caret belongs to a
    /// stream, and `ReviewDoc::tab` already names the stream the current
    /// gesture owns, so that settles it.
    fn caret_focused(&mut self, cx: &mut Cx) -> bool {
        let list = self.view.portal_list(cx, ids!(list));
        let Some(tab) = self.drawn_tab else {
            return false;
        };
        // An empty area answers `has_key_focus` for whatever is focused when
        // nothing is, so an instance that never drew would claim every key.
        let area = list.area();
        !matches!(area, Area::Empty)
            && cx.has_key_focus(area)
            && self
                .state()
                .read(|d| d.caret.is_some() && d.tab == tab && d.composer_tab != Some(tab))
    }

    /// The list's own selection and the caret are two claims about where typing
    /// goes; a keystroke settles it in the caret's favour. Then the edited file
    /// is lowered again, so its add/removed marks describe what you have now
    /// rather than what was on disk when it opened.
    ///
    /// Re-lowering runs here rather than on a timer: it is one histogram diff
    /// of one file. The tree-sitter re-parse behind it needs no debounce
    /// either. A request carries the `rev` it was made for and the worker drops
    /// one whose blob has moved on, so fast typing coalesces on its own.
    fn after_edit(&mut self, cx: &mut Cx) {
        if let Some(mut l) = self.view.portal_list(cx, ids!(list)).borrow_mut() {
            l.clear_selection(cx);
        }
        let git_dir = self.state().read(|d| d.git_dir.clone());
        let comments = crate::service::review_state(git_dir.as_deref())
            .load()
            .comments
            .clone();
        self.state().with(|d| relower_edited(d, &comments));
        self.redraw(cx);
    }

    /// Arrow keys, Home and End, and the keys that change text: Return, Tab,
    /// Backspace, Delete, undo and redo. Returns whether the caret took the key
    /// — which is what stops the list from scrolling on the same press.
    fn caret_keys(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        if !self.caret_focused(cx) {
            return false;
        }
        if ke.key_code == KeyCode::KeyS && (ke.modifiers.logo || ke.modifiers.control) {
            return self.save(cx);
        }
        if let Some(took) = self.state().with(|d| editor::edit_key(d, ke)) {
            if took {
                self.after_edit(cx);
            }
            return took;
        }
        let Some(tab) = self.drawn_tab else {
            return false;
        };
        let took = self.state().with(|d| editor::move_caret(d, tab, ke));
        if took {
            if let Some(mut l) = self.view.portal_list(cx, ids!(list)).borrow_mut() {
                l.clear_selection(cx);
            }
            self.redraw(cx);
        }
        took
    }

    fn find_keys(&mut self, cx: &mut Cx, ke: &KeyEvent) -> bool {
        let Some(tab) = self.drawn_tab else {
            return false;
        };
        let areas = [
            self.view.portal_list(cx, ids!(list)).area(),
            self.view.text_input(cx, ids!(find_input)).area(),
            self.view.text_input(cx, ids!(replace_input)).area(),
        ];
        let focused = areas
            .into_iter()
            .any(|area| !matches!(area, Area::Empty) && cx.has_key_focus(area));
        if !focused || !self.state().read(|d| d.tab == tab) {
            return false;
        }
        match ke.key_code {
            KeyCode::KeyF if ke.modifiers.logo || ke.modifiers.control => self.open_find(cx),
            // Escape closes the bar and leaves the caret where searching put
            // it, so you carry on from what you found.
            KeyCode::Escape if self.find.is_some() => {
                self.find = None;
                self.find_at = None;
                cx.set_key_focus(self.view.portal_list(cx, ids!(list)).area());
                self.redraw(cx);
                true
            }
            _ => false,
        }
    }

    /// Open the find bar and put the keyboard in it.
    fn open_find(&mut self, cx: &mut Cx) -> bool {
        self.find.get_or_insert_default();
        // Shown and focused from the draw, not here: a node that has never been
        // drawn does not resolve, so `set_visible` on a bar that starts hidden
        // is a silent no-op. Same reason the pinned header is always laid out
        // rather than toggled.
        self.focus_find = true;
        self.redraw(cx);
        true
    }

    /// Walk matches in this tab, unfolding and scrolling to the matching row.
    fn seek(&mut self, cx: &mut Cx, back: bool) {
        let Some(query) = self.find.clone().filter(|q| !q.is_empty()) else {
            return;
        };
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let hits = self.state().read(|d| editor::search_hits(d, tab, &query));
        let found = if back {
            hits.iter()
                .rev()
                .find(|hit| self.find_at.is_none_or(|at| **hit < at))
                .or_else(|| hits.last())
        } else {
            hits.iter()
                .find(|hit| self.find_at.is_none_or(|at| **hit > at))
                .or_else(|| hits.first())
        };
        let Some(&(row, byte)) = found else {
            return;
        };
        self.find_at = Some((row, byte));
        self.seek_row = Some(row);
        self.state().with(|d| {
            d.tab = tab;
            d.caret = editor::caret_at(d, tab, row, byte);
            d.selection_anchor = None;
            if let Some(path) = d.stream(tab)[..=row]
                .iter()
                .rev()
                .find_map(|row| match row {
                    Row::FileHeader { path, .. } => Some(path.clone()),
                    _ => None,
                })
            {
                d.folded.remove(&path);
            }
        });
        self.redraw(cx);
    }

    /// Replace the occurrence the caret sits on, or every one of them.
    fn replace(&mut self, cx: &mut Cx, with: &str, all: bool) {
        let Some(query) = self.find.clone().filter(|q| !q.is_empty()) else {
            return;
        };
        let Some(tab) = self.drawn_tab else {
            return;
        };
        let changed = self.state().with(|d| {
            if all {
                editor::replace_all(d, tab, &query, with)
            } else {
                editor::replace_one(d, tab, &query, with)
            }
        });
        if changed {
            self.after_edit(cx);
        }
    }

    /// Write the caret's buffer back to its file, and carry everything anchored
    /// to its old content across to the new hash.
    ///
    /// The two halves are kept apart: the arithmetic is worked out here as a
    /// plan over the document, and the service, which alone owns the store,
    /// performs the write and the anchor moves.
    fn save(&mut self, cx: &mut Cx) -> bool {
        let settings = self.state().read(|d| {
            let path = crate::theme::config_file()?;
            let blob = d.blobs.get(d.caret?.blob as usize)?;
            (blob.dirty() && blob.origin.as_ref() == Some(&path)).then(|| blob.text.clone())
        });
        if let Some(text) = settings {
            return self.apply_settings(cx, &text);
        }
        let Some(plan) = self.state().with(|d| save_plan(d, d.caret?.blob)) else {
            return false;
        };
        let sent = self.state().with(|d| {
            let Some(git_dir) = d.git_dir.clone() else {
                return false;
            };
            review().send(ReviewCmd::SaveFile {
                window: self.state().id,
                git_dir,
                plan,
            });
            true
        });
        if sent {
            self.redraw(cx);
        }
        sent
    }

    /// Saving the settings: parse them first, and only then let them land.
    /// A parse error is spliced in as the loud row the document already has
    /// for "this does not refer to anything real".
    fn apply_settings(&mut self, cx: &mut Cx, text: &str) -> bool {
        let applied = crate::theme::apply_settings_text(text);
        let error = applied.as_ref().err().map(ToString::to_string);
        self.state()
            .with(|d| editor::record_settings(d, text, error.as_deref()));
        if applied.is_ok() {
            cx.request_live_edit();
        }
        self.redraw(cx);
        true
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

/// Every occurrence of `query` in one line, as byte ranges. Case-insensitive on
/// ASCII — that is what you mean by "find" most of the time. Recomputed per row
/// per draw rather than cached: the text under it changes as you type, and a
/// cached hit would outlive the characters it named.
fn hits_in(text: &str, query: Option<&str>) -> Vec<(usize, usize)> {
    let Some(query) = query.filter(|q| !q.is_empty()) else {
        return Vec::new();
    };
    let (hay, needle) = (text.to_ascii_lowercase(), query.to_ascii_lowercase());
    let mut hits = Vec::new();
    let mut at = 0;
    while let Some(found) = hay[at..].find(&needle) {
        let start = at + found;
        hits.push((start, start + needle.len()));
        at = start + needle.len().max(1);
    }
    hits
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
        Band, CARD_END_EDGE, DrawnRow, FILE_HEADER_TOP_PADDING, STICKY_FADE, STICKY_HEIGHT,
        STICKY_TOP_GAP, anchor_entry, hits_in, row_at_y, sticky_offsets,
    };

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
    fn find_marks_every_occurrence_in_a_line_whatever_its_case() {
        assert_eq!(hits_in("let x = x + 1;", Some("x")), [(4, 5), (8, 9)]);
        assert_eq!(hits_in("Row::Code", Some("code")), [(5, 9)]);
        assert_eq!(hits_in("aaa", Some("aa")), [(0, 2)], "hits do not overlap");
        assert!(hits_in("anything", None).is_empty());
        assert!(
            hits_in("anything", Some("")).is_empty(),
            "an empty query finds nothing"
        );
        assert!(hits_in("anything", Some("zz")).is_empty());
    }

    /// Rows of different heights: one wrapped line is taller than its
    /// neighbours, the case a row delta could not express.
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
