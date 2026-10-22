//! The Output panel: one log per channel (language servers, the extension
//! host), picked from a dropdown like VS Code's.

use gpui_kit::component::menu::PopupMenuItem;

use super::*;

pub(super) const OUTPUT: usize = 1;

pub(super) struct Output {
    /// Each channel's lines, in the order they first wrote.
    channels: Vec<(String, Vec<String>)>,
    active: usize,
    list: ListState,
}

impl Output {
    pub(super) fn new() -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(200.));
        list.set_follow_mode(FollowMode::Tail);
        Self {
            channels: Vec::new(),
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
    /// Appends `text` to `channel`, creating it on first use.
    pub(super) fn log(&mut self, channel: &str, text: &str, cx: &mut Context<Self>) {
        let out = &mut self.output;
        let ix = match out.channels.iter().position(|(name, _)| name == channel) {
            Some(ix) => ix,
            None => {
                out.channels.push((channel.to_string(), Vec::new()));
                out.channels.len() - 1
            }
        };
        let lines = &mut out.channels[ix].1;
        let n = lines.len();
        lines.extend(text.lines().map(str::to_string));
        if ix == out.active {
            out.list.splice(n..n, lines.len() - n);
            cx.notify();
        }
    }

    pub(super) fn clear_output(&mut self, cx: &mut Context<Self>) {
        let out = &mut self.output;
        if let Some((_, lines)) = out.channels.get_mut(out.active) {
            lines.clear();
            out.show(out.active);
            cx.notify();
        }
    }

    /// The channel dropdown in the panel's title bar.
    pub(super) fn output_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (name, _) = self.output.channels.get(self.output.active)?;
        let names: Vec<_> = self
            .output
            .channels
            .iter()
            .map(|(n, _)| n.clone())
            .collect();
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
                for (ix, name) in names.iter().enumerate() {
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
