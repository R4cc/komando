//! The route page: while a Session reaches its Host through Jump Hosts, it shows one card per
//! Host on the way, starting from this PC. Green marks how far the connection has come; a
//! red card with an ✕ marks where it failed.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{Animation, AnimationExt as _, AnyElement, FontWeight, Hsla, IntoElement, ParentElement, Styled, div, px};

use crate::session::HopStep;
use crate::theme::{AppPalette, UI_FONT};
use crate::ui::widgets::{badge, icon_el};

/// One Host on the way, as its card shows it.
#[derive(Clone)]
pub struct RouteCard {
    pub name: String,
    /// `user@address:port`.
    pub detail: String,
    pub icon: String,
    pub color: Hsla,
}

/// Where a connection through Jump Hosts is.
pub struct ConnectRoute {
    /// The Hosts to sign in to, Jump Hosts first, the Host last.
    cards: Vec<RouteCard>,
    /// The hop being worked on.
    current: usize,
    step: HopStep,
    failed: Option<String>,
    /// The shell is open: the page fades out.
    finished: bool,
}

const CARD_WIDTH: f32 = 176.;
const LINK_WIDTH: f32 = 44.;
/// How long the page takes to fade out once the shell is open.
pub const FADE: Duration = Duration::from_millis(320);

impl ConnectRoute {
    pub fn new(cards: Vec<RouteCard>) -> Self {
        Self { cards, current: 0, step: HopStep::Reaching, failed: None, finished: false }
    }

    pub fn advance(&mut self, index: usize, step: HopStep) {
        if self.failed.is_none() {
            self.current = index.min(self.cards.len().saturating_sub(1));
            self.step = step;
        }
    }

    pub fn fail(&mut self, reason: String) {
        if !self.finished {
            self.failed = Some(reason);
        }
    }

    pub fn finish(&mut self) {
        self.finished = true;
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// The hop being worked on, by name (for prompts and the save-password offer).
    pub fn hop_name(&self, index: usize) -> Option<&str> {
        self.cards.get(index).map(|c| c.name.as_str())
    }

    pub fn current(&self) -> usize {
        self.current
    }

    fn state(&self, index: usize) -> CardState {
        if index < self.current || (index == self.current && self.step == HopStep::Done && !self.is_last(index)) {
            CardState::Done
        } else if index == self.current {
            if self.failed.is_some() { CardState::Failed } else { CardState::Active }
        } else {
            CardState::Waiting
        }
    }

    fn is_last(&self, index: usize) -> bool {
        index + 1 == self.cards.len()
    }

    fn step_text(&self, index: usize) -> &'static str {
        match self.state(index) {
            CardState::Done => "Signed in",
            CardState::Waiting => "Waiting",
            CardState::Failed => "Failed",
            CardState::Active => match self.step {
                HopStep::Reaching if index == 0 => "Connecting…",
                HopStep::Reaching => "Opening tunnel…",
                HopStep::CheckingKey => "Checking host key…",
                HopStep::SigningIn => "Signing in…",
                HopStep::Done => "Opening shell…",
            },
        }
    }

    /// The page. `actions` are the buttons shown when the connection failed; the current
    /// hop's question (Host Key, password) shows in the workspace's modal layer instead.
    pub fn render(&self, actions: Vec<AnyElement>, p: AppPalette) -> AnyElement {
        let target = self.cards.last().map(|c| c.name.clone()).unwrap_or_default();
        let jumps = self.cards.len().saturating_sub(1);
        let subtitle = match jumps {
            1 => "through 1 jump host".to_string(),
            n => format!("through {n} jump hosts"),
        };

        // This PC, then each hop with the link that leads to it. A link and its card stay
        // together when a narrow pane wraps the row.
        let mut row = vec![self.render_card(None, p)];
        for index in 0..self.cards.len() {
            row.push(
                div()
                    .flex()
                    .items_center()
                    .child(self.render_link(index, p))
                    .child(self.render_card(Some(index), p))
                    .into_any_element(),
            );
        }

        let page = div()
            .absolute()
            .inset_0()
            .rounded(px(crate::ui::widgets::ISLAND_RADIUS - 1.))
            .bg(p.background)
            .font_family(UI_FONT)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(28.))
            .px(px(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.text)
                            .child(format!("Connecting to {target}")),
                    )
                    .child(div().text_size(px(12.5)).text_color(p.text_muted).child(subtitle)),
            )
            .child(div().flex().flex_wrap().items_center().justify_center().gap_y(px(16.)).children(row))
            .when_some(self.failed.clone(), |el, reason| {
                el.child(
                    div()
                        .max_w(px(520.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(14.))
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(8.))
                                .px(px(12.))
                                .py(px(9.))
                                .rounded(px(8.))
                                .bg(p.danger.opacity(0.1))
                                .text_size(px(12.5))
                                .line_height(px(18.))
                                .text_color(p.danger)
                                .child(div().pt(px(2.)).child(icon_el("circle-alert", 14., p.danger)))
                                .child(reason),
                        )
                        .child(div().flex().gap(px(8.)).children(actions)),
                )
            });

        if self.finished {
            page.with_animation("route-fade", Animation::new(FADE), |el, delta| el.opacity(1. - delta))
                .into_any_element()
        } else {
            page.into_any_element()
        }
    }

    /// A card: `None` is this PC.
    fn render_card(&self, index: Option<usize>, p: AppPalette) -> AnyElement {
        let (name, detail, icon, color, state, step) = match index {
            None => ("This PC".to_string(), "Komando".to_string(), "monitor".to_string(), p.text_muted, CardState::Done, "Ready"),
            Some(i) => {
                let c = &self.cards[i];
                (c.name.clone(), c.detail.clone(), c.icon.clone(), c.color, self.state(i), self.step_text(i))
            }
        };
        let (border, step_color) = match state {
            CardState::Done => (p.success.opacity(0.45), p.success),
            CardState::Active => (p.success, p.success),
            CardState::Failed => (p.danger, p.danger),
            CardState::Waiting => (p.border, p.text_faint),
        };
        let mark = match state {
            CardState::Done => Some(("check", p.success)),
            CardState::Failed => Some(("x", p.danger)),
            _ => None,
        };
        let id = index.map_or(0, |i| i + 1);

        let card = div()
            .relative()
            .w(px(CARD_WIDTH))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(12.))
            .bg(p.surface)
            .border_1()
            .border_color(border)
            .when(state == CardState::Waiting, |el| el.opacity(0.5))
            .when(state == CardState::Failed, |el| el.bg(p.danger.opacity(0.06)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(badge(&icon, color, 30.))
                    .children(mark.map(|(icon, color)| {
                        div()
                            .size(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(color)
                            .child(icon_el(icon, 13., p.surface))
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .min_w_0()
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.text).truncate().child(name))
                    .child(div().text_size(px(11.5)).text_color(p.text_muted).truncate().child(detail)),
            )
            .child(
                div()
                    .text_size(px(11.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(step_color)
                    .child(step),
            );

        if state != CardState::Active {
            return card.into_any_element();
        }
        // The card being worked on breathes: a green ring that fades in and out around it.
        div()
            .relative()
            .child(
                div()
                    .absolute()
                    .top(px(-4.))
                    .left(px(-4.))
                    .right(px(-4.))
                    .bottom(px(-4.))
                    .rounded(px(15.))
                    .border_2()
                    .border_color(p.success)
                    .with_animation(
                        ("route-pulse", id),
                        Animation::new(Duration::from_millis(1400)).repeat().with_easing(gpui::pulsating_between(0.15, 0.7)),
                        |el, delta| el.opacity(delta),
                    ),
            )
            .child(card)
            .into_any_element()
    }

    /// The link that leads to hop `index`: green once passed, with a dot travelling along it
    /// while the connection is on its way there.
    fn render_link(&self, index: usize, p: AppPalette) -> AnyElement {
        let state = self.state(index);
        let travelling = state == CardState::Active && self.step == HopStep::Reaching;
        let color = match state {
            CardState::Done | CardState::Active => p.success,
            CardState::Failed => p.danger,
            CardState::Waiting => p.border,
        };
        div()
            .relative()
            .w(px(LINK_WIDTH))
            .h(px(2.))
            .mx(px(6.))
            .rounded_full()
            .bg(if travelling { p.success.opacity(0.3) } else { color })
            .when(travelling, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .size(px(8.))
                        .rounded_full()
                        .bg(p.success)
                        .with_animation(
                            ("route-dot", index),
                            Animation::new(Duration::from_millis(900)).repeat().with_easing(gpui::ease_in_out),
                            |el, delta| el.left(px(delta * (LINK_WIDTH - 8.))),
                        ),
                )
            })
            .into_any_element()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CardState {
    Done,
    Active,
    Failed,
    Waiting,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(hops: usize) -> ConnectRoute {
        let card = |n: usize| RouteCard { name: format!("h{n}"), detail: String::new(), icon: "server".into(), color: gpui::black() };
        ConnectRoute::new((0..hops).map(card).collect())
    }

    #[test]
    fn hops_before_the_current_one_are_done() {
        let mut r = route(3);
        r.advance(1, HopStep::SigningIn);
        assert!(r.state(0) == CardState::Done);
        assert!(r.state(1) == CardState::Active);
        assert!(r.state(2) == CardState::Waiting);
        assert_eq!(r.step_text(1), "Signing in…");
    }

    #[test]
    fn a_signed_in_jump_host_is_done_but_the_host_waits_for_its_shell() {
        let mut r = route(2);
        r.advance(0, HopStep::Done);
        assert!(r.state(0) == CardState::Done);
        r.advance(1, HopStep::Done);
        assert!(r.state(1) == CardState::Active);
        assert_eq!(r.step_text(1), "Opening shell…");
    }

    #[test]
    fn a_failure_marks_the_current_hop_and_stops_progress() {
        let mut r = route(3);
        r.advance(1, HopStep::Reaching);
        r.fail("Timed out".into());
        r.advance(2, HopStep::Reaching);
        assert!(r.state(1) == CardState::Failed);
        assert!(r.state(2) == CardState::Waiting);
    }
}
