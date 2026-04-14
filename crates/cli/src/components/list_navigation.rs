use ratatui::layout::Rect;
use tui_widget_list::{ListState, hit_test::Hit};

pub fn scroll_list(list_state: &mut ListState, viewport: Rect, delta: i16) {
    if delta == 0 || viewport.width == 0 || viewport.height == 0 {
        return;
    }

    for _ in 0..delta.unsigned_abs() {
        if let Some(index) = edge_visible_item(list_state, viewport, delta > 0) {
            list_state.select(Some(index));
        }

        if delta > 0 {
            list_state.next();
        } else {
            list_state.previous();
        }
    }
}

fn edge_visible_item(list_state: &ListState, viewport: Rect, forward: bool) -> Option<usize> {
    let x = viewport.left() + viewport.width.saturating_sub(1) / 2;
    let mut rows = viewport.top()..viewport.bottom();

    if forward {
        rows.rev().find_map(|y| match list_state.hit_test(x, y) {
            Some(Hit::Item(index)) => Some(index),
            _ => None,
        })
    } else {
        rows.find_map(|y| match list_state.hit_test(x, y) {
            Some(Hit::Item(index)) => Some(index),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{
        buffer::Buffer,
        widgets::{StatefulWidget, Widget},
    };
    use tui_widget_list::{ListBuilder, ListView};

    use super::*;

    fn render_list(state: &mut ListState, area: Rect, item_sizes: &[u16]) {
        let builder = ListBuilder::new(|context| {
            (
                DummyWidget {
                    height: item_sizes[context.index],
                },
                item_sizes[context.index],
            )
        });
        let list = ListView::new(builder, item_sizes.len()).infinite_scrolling(false);
        let mut buf = Buffer::empty(area);
        list.render(area, &mut buf, state);
    }

    #[test]
    fn scrolls_one_row_for_uniform_items() {
        let area = Rect::new(0, 0, 10, 3);
        let mut state = ListState::default();
        let item_sizes = [1, 1, 1, 1, 1];

        render_list(&mut state, area, &item_sizes);
        scroll_list(&mut state, area, 1);
        render_list(&mut state, area, &item_sizes);

        assert_eq!(state.scroll_offset_index(), 1);
    }

    #[test]
    fn scrolls_within_large_items() {
        let area = Rect::new(0, 0, 10, 3);
        let mut state = ListState::default();
        let item_sizes = [4, 1];

        render_list(&mut state, area, &item_sizes);
        scroll_list(&mut state, area, 1);
        render_list(&mut state, area, &item_sizes);

        assert_eq!(state.scroll_offset_index(), 0);
        assert_eq!(state.scroll_truncation(), 1);
    }

    struct DummyWidget {
        height: u16,
    }

    impl Widget for DummyWidget {
        fn render(self, area: Rect, buf: &mut Buffer) {
            for y in area.top()..area.top().saturating_add(self.height).min(area.bottom()) {
                buf[(area.left(), y)].set_symbol("x");
            }
        }
    }
}
