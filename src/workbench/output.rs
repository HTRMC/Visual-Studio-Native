//! The Output panel: one log per channel (language servers, the extension
//! host), picked from a dropdown like VS Code's.

use gpui_kit::component::menu::PopupMenuItem;

use super::*;

pub(super) const OUTPUT: usize = 1;

pub(super) struct Output {
    /// Each channel's lines, in the order they first wrote.
    channels: Vec<(String, Vec<String>)>,
    /// Channels whose last line an `append` left open.
    open: Vec<String>,
    active: usize,
    list: ListState,
}

impl Output {
    pub(super) fn new() -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(200.));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            channels: Vec::new(),
            open: Vec::new(),
            active: 0,
            list,
        }
    }

    fn show(&mut self, ix: usize) {
        self.active = ix;
        self.list.reset(self.channels[ix].1.len());
        self.list.set_follow_mode(FollowMode::Tail);
    }
}

impl Workbench {
    /// Appends `text` to `channel` as lines, creating it on first use.
    pub(super) fn log(&mut self, channel: &str, text: &str, cx: &mut Context<Self>) {
        let line = text.trim_end_matches(['\r', '\n']);
        self.append_output(channel, &format!("{line}\n"), cx);
    }

    /// An output channel's `append`: `text` goes on the end of an open last
    /// line, and a last line without a newline stays open.
    pub(super) fn append_output(&mut self, channel: &str, text: &str, cx: &mut Context<Self>) {
        let out = &mut self.output;
        let ix = match out.channels.iter().position(|(name, _)| name == channel) {
            Some(ix) => ix,
            None => {
                out.channels.push((channel.to_string(), Vec::new()));
                out.channels.len() - 1
            }
        };
        let was_open = out.open.iter().any(|c| c == channel);
        out.open.retain(|c| c != channel);
        let lines = &mut out.channels[ix].1;
        let n = lines.len();
        if push_text(lines, was_open, text) {
            out.open.push(channel.to_string());
        }
        if ix == out.active {
            // The joined line changed too.
            let from = n.saturating_sub(was_open as usize).min(lines.len());
            out.list.splice(from..n, lines.len() - from);
            cx.notify();
        }
    }

    /// An output channel's `clear`.
    pub(super) fn clear_channel(&mut self, channel: &str, cx: &mut Context<Self>) {
        let out = &mut self.output;
        out.open.retain(|c| c != channel);
        if let Some(ix) = out.channels.iter().position(|(name, _)| name == channel) {
            out.channels[ix].1.clear();
            if ix == out.active {
                out.show(ix);
            }
            cx.notify();
        }
    }

    /// An output channel's `show`: the Output panel on that channel.
    pub(super) fn show_channel(&mut self, channel: &str, cx: &mut Context<Self>) {
        if let Some(ix) = self.output.channels.iter().position(|(name, _)| name == channel) {
            self.output.show(ix);
        }
        self.panel = true;
        self.panel_tab = OUTPUT;
        cx.notify();
    }

    pub(super) fn clear_output(&mut self, cx: &mut Context<Self>) {
        let out = &mut self.output;
        if let Some((name, lines)) = out.channels.get_mut(out.active) {
            out.open.retain(|c| c != name);
            lines.clear();
            out.show(out.active);
            cx.notify();
        }
    }

    /// The channel dropdown in the panel's title bar.
    pub(super) fn output_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (name, _) = self.output.channels.get(self.output.active)?;
        // Listed by name, as VS Code sorts them.
        let mut names: Vec<_> = (self.output.channels.iter().enumerate())
            .map(|(ix, (n, _))| (ix, n.clone()))
            .collect();
        names.sort_by_key(|(_, n)| n.to_lowercase());
        let wb = cx.weak_entity();
        let button = Button::new("output-channels")
            .ghost()
            .compact()
            .text_size(px(12.))
            .text_color(col::fg())
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(name.clone())
                    .child(codicon(icon::CHEVRON_DOWN)),
            )
            .dropdown_menu(move |mut menu, _, _| {
                for &(ix, ref name) in &names {
                    let wb = wb.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                        wb.update(cx, |this, cx| {
                            this.output.show(ix);
                            cx.notify();
                        })
                        .ok();
                    }));
                }
                menu
            });
        Some(button.into_any_element())
    }

    pub(super) fn render_output(&self, cx: &Context<Self>) -> AnyElement {
        // Only the visible lines are laid out; it follows the tail.
        let wb = cx.weak_entity();
        let lines = list(self.output.list.clone(), move |ix, _, cx| {
            let line = wb.upgrade().and_then(|wb| {
                let out = &wb.read(cx).output;
                let text = out.channels.get(out.active)?.1.get(ix)?.clone();
                Some(div().min_h(px(19.)).child(text))
            });
            line.unwrap_or_else(div).into_any_element()
        });
        div()
            .size_full()
            .px(px(20.))
            .font_family("Consolas")
            .text_size(px(14.))
            .child(lines.size_full())
            .into_any_element()
    }
}

/// Adds `text` to `lines`, its first piece joining the last line when that's
/// `open`. Returns whether the last line is open now (no final newline).
fn push_text(lines: &mut Vec<String>, open: bool, text: &str) -> bool {
    let mut parts = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    if open && let (Some(line), Some(first)) = (lines.last_mut(), parts.next()) {
        line.push_str(first);
    }
    lines.extend(parts.map(str::to_string));
    // The split's empty piece after a final newline isn't a line.
    let closed = text.ends_with('\n');
    if closed {
        lines.pop();
    }
    !closed
}

#[cfg(test)]
mod tests {
    use super::push_text;

    #[test]
    fn append() {
        let mut lines = Vec::new();
        let open = push_text(&mut lines, false, "a");
        let open = push_text(&mut lines, open, "b\r\nc\n");
        assert!(!open);
        assert_eq!(lines, ["ab", "c"]);
        assert!(push_text(&mut lines, open, "d"));
        assert_eq!(lines, ["ab", "c", "d"]);
    }
}
