//! The floating thread details card at the canvas' top right
//! (`chat/ThreadDetailsCard.tsx`, `ThreadDetailsPanel.tsx`). Only rows backed by
//! real data render: environment, branch, the parent thread, usage limits and context.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, Context, ElementId, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div,
    hsla, point, px, relative, rgb,
};

use super::titlebar::{RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace};
use crate::model::{ThreadShell, TokenUsage, UsageLimits};
use crate::theme::{self, Tokens};

const CARD_WIDTH: f32 = 280.;
const CARD_INSET: f32 = 12.;
const ROW_HEIGHT: f32 = 32.;
const METER_ROW_HEIGHT: f32 = 24.;

/// The card's opaque stand-ins for `dropdown-glass`; GPUI has no backdrop blur.
struct CardColors {
    background: Hsla,
    border: Hsla,
    divider: Hsla,
    hover: Hsla,
    label: Hsla,
    muted: Hsla,
    track: Hsla,
    tick: Hsla,
}

impl CardColors {
    fn new(tokens: &Tokens) -> Self {
        let (background, border, divider, hover) = if tokens.dark {
            (rgb(0x0f0f0f).into(), rgb(0x252525).into(), hsla(0., 0., 1., 0.052), hsla(0., 0., 1., 0.08))
        } else {
            (rgb(0xfefefe).into(), rgb(0xe8e8e8).into(), rgb(0xeeeef0).into(), hsla(0., 0., 0., 0.035))
        };
        Self {
            background,
            border,
            divider,
            hover,
            label: tokens.foreground.opacity(0.8),
            muted: tokens.muted_foreground,
            tick: tokens.foreground.opacity(0.6),
            track: tokens.muted_foreground.opacity(if tokens.dark { 0.2 } else { 0.3 }),
        }
    }
}

/// One label | bar | value row of the usage grid (`ThreadDetailsUsage.tsx` `UsageRow`).
struct Meter {
    id: SharedString,
    label: SharedString,
    /// Bar fill from 0 to 100; `None` hides the bar.
    fill: Option<f32>,
    fill_color: Hsla,
    /// Auto-compaction marker from 0 to 100.
    tick: Option<f32>,
    value: SharedString,
    value_color: Hsla,
    tooltip: SharedString,
}

impl Workspace {
    /// Whether the card is on screen, inline or as the popover.
    pub(super) fn details_shown(&self, window: &Window) -> bool {
        self.target.is_some() && if self.details_inline(window) { self.details_open } else { self.details_popover }
    }

    /// The toggle drives whichever presentation the canvas allows.
    pub(super) fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_inline(window) {
            self.details_open = !self.details_open;
        } else {
            self.details_popover = !self.details_popover;
        }
        cx.notify();
    }

    pub(super) fn escape_details_popover(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.details_popover && event.keystroke.key == "escape" {
            self.details_popover = false;
            cx.stop_propagation();
            cx.notify();
        }
    }

    pub(super) fn render_details_card(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.details_shown(window) {
            return None;
        }
        let tokens = *theme::tokens(cx);
        let colors = CardColors::new(&tokens);
        let sections: Vec<AnyElement> = [
            self.workspace_section(&tokens, &colors),
            self.branch_section(&tokens, &colors),
            self.lineage_section(&tokens, &colors, cx),
            self.usage_section(&tokens, &colors),
            self.context_section(&tokens, &colors),
        ]
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, section)| {
            v_flex()
                .px_2()
                .pt_2()
                .pb(px(10.))
                .when(index > 0, |el| el.border_t_1().border_color(colors.divider))
                .child(section)
                .into_any_element()
        })
        .collect();
        let card = v_flex()
            .id("thread-details")
            .w_full()
            .overflow_y_scroll()
            .occlude()
            .rounded(px(16.))
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .children(sections);
        if self.details_inline(window) {
            return Some(
                div()
                    .absolute()
                    .top(px(CARD_INSET))
                    .bottom(px(CARD_INSET))
                    .right(px(CARD_INSET))
                    .w(px(CARD_WIDTH))
                    .child(card.max_h_full())
                    .into_any_element(),
            );
        }
        // `PopoverPopup variant="panel"` under the header, `align="end"`: 4px align
        // offset plus its 8px padding put the card where the inline one sits, 8px
        // below the header, capped at `100dvh - 6.5rem`.
        let viewport = window.viewport_size();
        let corner = point(viewport.width - RIGHT_RESIZE_INSET - px(CARD_INSET), TITLEBAR_HEIGHT + px(8.));
        Some(
            deferred(
                anchored().anchor(Anchor::TopRight).position(corner).child(
                    div()
                        .w(px(CARD_WIDTH))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.details_popover = false;
                            cx.notify();
                        }))
                        .child(card.max_h(viewport.height - px(104.))),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The environment row: machine icon, environment label, workspace kind at the far right.
    fn workspace_section(&self, tokens: &Tokens, colors: &CardColors) -> Option<AnyElement> {
        let label = self.server_info.environment_label.clone()?;
        let machine = match self.server_info.machine_kind() {
            "desktop" | "mac-studio" | "mac-mini" => "icons/monitor.svg",
            "laptop" => "icons/laptop.svg",
            "cloud" => "icons/cloud.svg",
            _ => "icons/server.svg",
        };
        let worktree = self.open_projection().and_then(|p| p.thread.worktree_path.clone());
        let workspace_root = self.target_project().map(|project| project.workspace_root.clone());
        let (workspace_icon, workspace_path) = match worktree {
            Some(path) => ("icons/folder-git.svg", Some(path)),
            None => ("icons/folder.svg", workspace_root),
        };
        // The locked row's label stays muted in dark mode (`BranchToolbar.tsx`).
        let label_color = if tokens.dark { tokens.muted_foreground } else { colors.label };
        Some(
            row(colors)
                .child(icon(machine).with_size(px(18.)).mx(px(-1.)).text_color(tokens.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().text_color(label_color).child(label))
                .child(
                    div()
                        .id("details-workspace")
                        .flex_none()
                        .child(icon(workspace_icon).with_size(px(16.)).text_color(tokens.muted_foreground))
                        .when_some(workspace_path, |el, path| {
                            let path = SharedString::from(path);
                            el.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                        }),
                )
                .into_any_element(),
        )
    }

    fn branch_section(&self, tokens: &Tokens, colors: &CardColors) -> Option<AnyElement> {
        let branch = self.open_projection()?.thread.branch.clone()?;
        Some(
            row(colors)
                .child(icon("icons/git-branch.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().child(branch))
                .into_any_element(),
        )
    }

    /// `ThreadRelationshipsPanel` from the shell: the parent thread, running
    /// subagents, and settled ones folded under "Previous agents".
    fn lineage_section(&self, tokens: &Tokens, colors: &CardColors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Target::Thread(id)) = &self.target else { return None };
        let thread = self.shell.threads.iter().find(|thread| &thread.id == id)?;
        let parent = thread.lineage.parent_thread_id.as_ref().and_then(|parent| self.thread_shell(parent));
        let (previous, active): (Vec<&ThreadShell>, Vec<&ThreadShell>) = self
            .shell
            .threads
            .iter()
            .filter(|child| {
                child.lineage.parent_thread_id.as_ref() == Some(id)
                    && child.lineage.relationship_to_parent.as_deref() == Some("subagent")
            })
            .partition(|child| is_settled_status(&child.status));
        if parent.is_none() && active.is_empty() && previous.is_empty() {
            return None;
        }
        let relationship_row = |thread: &ThreadShell, path: &'static str, cx: &mut Context<Self>| {
            let thread_id = thread.id.clone();
            row(colors)
                .id(ElementId::Name(format!("details-lineage-{thread_id}").into()))
                .cursor_pointer()
                .hover(|style| style.bg(colors.hover))
                .child(icon(path).with_size(px(16.)).text_color(tokens.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .line_height(px(16.))
                        .text_color(tokens.foreground.opacity(0.85))
                        .child(thread.title.clone()),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.select_thread(thread_id.clone(), window, cx)))
        };
        let heading: SharedString = match active.len() {
            0 => "Lineage".into(),
            running => format!("Lineage · {running} running").into(),
        };
        let mut section = v_flex()
            // A lone "Previous agents" row names itself.
            .when(parent.is_some() || !active.is_empty(), |el| el.child(section_heading(heading, tokens)));
        if let Some(parent) = parent {
            section = section.child(relationship_row(parent, "icons/corner-left-up.svg", cx));
        }
        for child in &active {
            section = section.child(relationship_row(child, "icons/bot.svg", cx));
        }
        if !previous.is_empty() {
            let expanded = self.lineage_expanded.as_ref() == Some(id);
            let thread_id = id.clone();
            section = section.child(
                row(colors)
                    .id("details-previous-agents")
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.hover))
                    .child(icon("icons/bot.svg").with_size(px(16.)).text_color(tokens.muted_foreground))
                    .child(div().flex_1().min_w_0().truncate().child("Previous agents"))
                    .when(!expanded, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::NORMAL)
                                .text_color(tokens.muted_foreground)
                                .child(previous.len().to_string()),
                        )
                    })
                    .child(
                        icon("icons/chevron-down.svg")
                            .with_size(px(16.))
                            .text_color(tokens.muted_foreground)
                            .when(!expanded, |icon| icon.rotate(gpui_kit::radians(-std::f32::consts::FRAC_PI_2))),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.lineage_expanded =
                            (this.lineage_expanded.as_ref() != Some(&thread_id)).then(|| thread_id.clone());
                        cx.notify();
                    })),
            );
            if expanded {
                for child in &previous {
                    section = section.child(relationship_row(child, "icons/bot.svg", cx));
                }
            }
        }
        Some(section.into_any_element())
    }

    fn thread_shell(&self, id: &str) -> Option<&ThreadShell> {
        self.shell.threads.iter().find(|thread| thread.id == id)
    }

    /// The selected provider's limit windows; each bar reads as room left.
    fn usage_section(&self, tokens: &Tokens, colors: &CardColors) -> Option<AnyElement> {
        let selection = self.composer_model()?;
        let limits = self.server_info.provider(&selection.instance_id)?.usage_limits.as_ref()?;
        Some(meter_grid(limit_meters(limits, tokens), colors))
    }

    fn context_section(&self, tokens: &Tokens, colors: &CardColors) -> Option<AnyElement> {
        let usage = self.open_projection()?.context_usage()?;
        Some(meter_grid(vec![context_meter(usage, tokens)], colors))
    }

    fn open_projection(&self) -> Option<&crate::model::ThreadProjection> {
        match &self.target {
            Some(Target::Thread(_)) => self.thread.as_ref()?.state.projection.as_ref(),
            _ => None,
        }
    }

    fn target_project(&self) -> Option<&crate::model::ProjectShell> {
        let project_id = match self.target.as_ref()? {
            Target::Thread(id) => &self.shell.threads.iter().find(|thread| &thread.id == id)?.project_id,
            Target::Draft { project_id } => project_id,
        };
        self.shell.projects.iter().find(|project| &project.id == project_id)
    }
}

/// The statuses `ThreadRelationshipsControl.tsx` files under "Previous agents".
fn is_settled_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "error" | "cancelled" | "interrupted" | "idle")
}

/// `THREAD_DETAILS_PANEL_LOCKED_ROW_CLASS`: 32px, 10px inset and gap, 13px medium.
fn row(colors: &CardColors) -> gpui_kit::Div {
    h_flex()
        .h(px(ROW_HEIGHT))
        .w_full()
        .flex_none()
        .rounded(px(8.))
        .gap(px(10.))
        .px(px(10.))
        .text_size(px(13.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(colors.label)
}

fn section_heading(title: SharedString, tokens: &Tokens) -> impl IntoElement {
    h_flex()
        .mb_1()
        .min_h(px(32.))
        .px(px(10.))
        .text_size(px(11.))
        .line_height(px(16.))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(tokens.muted_foreground)
        .child(title)
}

fn limit_meters(limits: &UsageLimits, tokens: &Tokens) -> Vec<Meter> {
    if limits.unavailable.is_some() || limits.windows.is_empty() {
        return vec![Meter {
            id: "usage-unavailable".into(),
            label: "Limits".into(),
            fill: None,
            fill_color: tokens.foreground,
            tick: None,
            value: "Unavailable".into(),
            value_color: tokens.muted_foreground,
            tooltip: "Could not read limits.".into(),
        }];
    }
    limits
        .windows
        .iter()
        .map(|window| {
            let left = window.remaining_percent();
            let (fill_color, value_color) = tone(left <= 10, left <= 20, tokens.foreground.opacity(0.8), tokens);
            Meter {
                id: format!("usage-{}", window.id).into(),
                label: window.label.clone().into(),
                fill: Some(left as f32),
                fill_color,
                tick: None,
                value: format!("{left}%").into(),
                value_color,
                tooltip: format!("{}: {left}% left", window.label).into(),
            }
        })
        .collect()
}

/// `ThreadDetailsContext`: the bar fills as context is used; the tick marks auto-compaction.
fn context_meter(usage: &TokenUsage, tokens: &Tokens) -> Meter {
    let max = usage.max_tokens.filter(|max| *max > 0);
    let used = max.map(|max| (usage.used_tokens as f64 / max as f64 * 100.).clamp(0., 100.).round() as u32);
    let tick = max
        .zip(usage.auto_compact_threshold)
        .filter(|(max, threshold)| *threshold > 0 && threshold < max)
        .map(|(max, threshold)| threshold as f32 / max as f32 * 100.);
    let used_label = format_context_tokens(usage.used_tokens);
    let value = match max {
        Some(max) => format!("{used_label} / {}", format_context_tokens(max)),
        None => used_label.clone(),
    };
    let tooltip = match (used, max) {
        (Some(used), Some(max)) => {
            format!("Context: {used}% used · {} of {} tokens", group_thousands(usage.used_tokens), group_thousands(max))
        }
        _ => format!("Context: {used_label} tokens used"),
    };
    let (fill_color, value_color) = match used {
        Some(used) => tone(used >= 90, used >= 80, tokens.muted_foreground, tokens),
        None => (tokens.muted_foreground, tokens.muted_foreground),
    };
    Meter {
        id: "usage-context".into(),
        label: "Context".into(),
        fill: used.map(|used| used as f32),
        fill_color,
        tick,
        value: value.into(),
        value_color,
        tooltip: tooltip.into(),
    }
}

/// Error and warning tones for a meter's (fill, value) colors.
fn tone(error: bool, warning: bool, fill: Hsla, tokens: &Tokens) -> (Hsla, Hsla) {
    if error {
        (tokens.error, tokens.error_foreground)
    } else if warning {
        (tokens.warning, tokens.warning_foreground)
    } else {
        (fill, tokens.foreground.opacity(0.8))
    }
}

/// The `grid-cols-[auto_minmax(3rem,1fr)_auto] gap-x-3` grid. Columns are
/// flex columns, so every row shares the widest label and value; hover rows
/// sit underneath and own the tooltips.
fn meter_grid(meters: Vec<Meter>, colors: &CardColors) -> AnyElement {
    let hover = colors.hover;
    let rows = meters.iter().enumerate().map(|(index, meter)| {
        let tooltip = meter.tooltip.clone();
        div()
            .id(ElementId::Name(meter.id.clone()))
            .absolute()
            .top(px(index as f32 * METER_ROW_HEIGHT))
            .left_0()
            .right_0()
            .h(px(METER_ROW_HEIGHT))
            .rounded(px(8.))
            .hover(|style| style.bg(hover))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
    });
    let cell = || h_flex().h(px(METER_ROW_HEIGHT));
    let labels = meters.iter().map(|meter| {
        cell().max_w(px(112.)).child(div().min_w_0().truncate().text_color(colors.muted).child(meter.label.clone()))
    });
    let track = colors.track;
    let bars = meters.iter().map(|meter| {
        cell().w_full().children(meter.fill.map(|fill| {
            div()
                .relative()
                .w_full()
                .h(px(3.))
                .rounded_full()
                .bg(track)
                .when(fill > 0., |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left_0()
                            .w(relative(fill / 100.))
                            .min_w(px(6.))
                            .rounded_full()
                            .bg(meter.fill_color),
                    )
                })
                .children(meter.tick.map(|tick| {
                    div()
                        .absolute()
                        .top(px(-3.))
                        .left(relative(tick / 100.))
                        .ml(px(-0.5))
                        .w(px(1.))
                        .h(px(9.))
                        .bg(colors.tick)
                }))
        }))
    });
    let values = meters.iter().map(|meter| {
        cell()
            .justify_end()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(meter.value_color)
            .child(meter.value.clone())
    });
    div()
        .relative()
        .children(rows)
        .child(
            h_flex()
                .px(px(10.))
                .gap_3()
                .text_xs()
                .child(v_flex().flex_none().children(labels))
                .child(v_flex().flex_1().min_w(px(48.)).children(bars))
                .child(v_flex().flex_none().items_end().children(values)),
        )
        .into_any_element()
}

/// `formatContextWindowTokens` in `apps/web/src/lib/contextWindow.ts`.
fn format_context_tokens(value: u64) -> String {
    let trim = |text: String| text.strip_suffix(".0").map(str::to_owned).unwrap_or(text);
    match value {
        0..1_000 => value.to_string(),
        1_000..10_000 => trim(format!("{:.1}", value as f64 / 1_000.)) + "k",
        10_000..999_500 => format!("{}k", (value as f64 / 1_000.).round()),
        _ => trim(format!("{:.1}", value as f64 / 1_000_000.)) + "m",
    }
}

/// `toLocaleString("en-US")` for an integer.
fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_tokens_format_like_the_web() {
        let cases = [
            (999, "999"),
            (1_000, "1k"),
            (1_234, "1.2k"),
            (9_960, "10k"),
            (10_000, "10k"),
            (345_400, "345k"),
            (999_499, "999k"),
            (999_500, "1m"),
            (1_000_000, "1m"),
            (1_500_000, "1.5m"),
        ];
        for (value, expected) in cases {
            assert_eq!(format_context_tokens(value), expected, "{value}");
        }
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(7), "7");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(345_000), "345,000");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }
}
