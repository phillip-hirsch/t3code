//! The review guide as a tour (`GuideTour`, `GuideRail`, `GuideStop`, `GuideExcerpt`,
//! `GuideNotes`): one stop at a time with its excerpts from the change, lettered notes on their
//! lines, the other files it touches, questions to hand an agent, and the way to the next stop.

use crate::pull_request_detail::{Guide, GuideExcerpt, GuideStop};
use crate::pull_request_diff::{self as diff_logic, Row};

use super::*;

/// `@min-[56rem]/guide`: from this panel width the stops list stays beside the stop.
const RAIL_BESIDE: f32 = 896.;

impl Workspace {
    /// The tour of a written guide, under its banner.
    pub(super) fn render_pr_guide_tour(
        &mut self,
        guide: &Guide,
        banner: Option<AnyElement>,
        width: f32,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Progress belongs to one written guide (`guideProgressKey`): a rewrite starts over.
        if let Some(panel) = self.pull_requests.panel.as_mut()
            && panel.guide_head.as_deref() != Some(guide.head_sha.as_str())
        {
            panel.guide_head = Some(guide.head_sha.clone());
            panel.guide_position = 0;
            panel.guide_seen.clear();
        }
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let count = guide.stops.len();
        let position = panel.guide_position.min(count.saturating_sub(1));
        let seen = panel.guide_seen.clone();
        let rail_open = panel.guide_rail_open;
        let beside = width >= RAIL_BESIDE;
        let rail = self.render_pr_guide_rail(guide, position, &seen, beside, t, cx);
        let main = match guide.stops.get(position) {
            None => div()
                .text_sm()
                .text_color(t.muted_foreground)
                .child("This change has no files to tour.")
                .into_any_element(),
            Some(stop) => {
                let next = guide.stops.get(position + 1).map(|s| s.title.clone());
                let list =
                    (!beside && rail_open).then(|| self.render_pr_guide_rail(guide, position, &seen, false, t, cx));
                self.render_pr_guide_stop(stop, position, count, next, beside, list, t, cx)
            }
        };
        let column = v_flex().flex_1().min_w_0().gap(px(16.)).children(banner).child(main);
        div()
            .id("pr-guide")
            .size_full()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .items_start()
                    .w_full()
                    .max_w(px(1600.))
                    .mx_auto()
                    .gap(px(24.))
                    .px(px(16.))
                    .py(px(16.))
                    .when(beside, |el| el.child(div().w(px(256.)).flex_none().child(rail)))
                    .child(column),
            )
            .into_any_element()
    }

    fn set_pr_guide_stop(&mut self, next: usize, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if next > panel.guide_position {
            panel.guide_seen.insert(panel.guide_position);
        }
        panel.guide_position = next;
        panel.guide_rail_open = false;
        cx.notify();
    }

    /// `GuideRail`: every stop, read ones ticked.
    fn render_pr_guide_rail(
        &self,
        guide: &Guide,
        position: usize,
        seen: &HashSet<usize>,
        beside: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows: Vec<AnyElement> = guide
            .stops
            .iter()
            .enumerate()
            .map(|(index, stop)| {
                let active = index == position;
                let read = seen.contains(&index);
                let bullet = div()
                    .size(px(20.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .text_size(px(10.))
                    .map(|el| {
                        if read {
                            el.border_color(Hsla { a: 0.4, ..t.success_foreground })
                                .text_color(t.success_foreground)
                                .child(Icon::new(Lucide::Check).with_size(px(12.)))
                        } else {
                            el.border_color(t.border).text_color(t.muted_foreground).child((index + 1).to_string())
                        }
                    });
                h_flex()
                    .id(ElementId::Name(format!("pr-guide-rail-{index}").into()))
                    .w_full()
                    .items_start()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .map(|el| {
                        if active {
                            el.bg(t.accent)
                        } else {
                            el.hover(|s| s.bg(Hsla { a: t.accent.a * 0.5, ..t.accent }))
                        }
                    })
                    .child(bullet)
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(2.))
                            .child(div().text_sm().text_color(t.foreground).child(stop.title.clone()))
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap(px(6.))
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .when(stop.attention.is_some(), |el| {
                                        el.child(
                                            div()
                                                .px(px(6.))
                                                .rounded(px(4.))
                                                .bg(Hsla { a: 0.12, ..t.warning })
                                                .text_color(t.warning_foreground)
                                                .child("Look closely"),
                                        )
                                    })
                                    .child(logic::stop_subline(stop)),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.set_pr_guide_stop(index, cx)))
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap(px(16.))
            .when(beside, |el| {
                el.child(div().text_xs().text_color(t.muted_foreground).child(format!(
                    "{} of {} read",
                    seen.len(),
                    guide.stops.len()
                )))
            })
            .child(v_flex().gap(px(2.)).children(rows))
            .into_any_element()
    }

    /// `GuideStop`.
    fn render_pr_guide_stop(
        &mut self,
        stop: &GuideStop,
        index: usize,
        count: usize,
        next: Option<String>,
        beside: bool,
        list: Option<AnyElement>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let counter = format!("Stop {} of {count}", index + 1);
        let header = h_flex()
            .justify_between()
            .gap(px(8.))
            .child(if beside {
                div().text_xs().text_color(t.muted_foreground).child(counter).into_any_element()
            } else {
                ghost_button("pr-guide-stops", t)
                    .child(counter)
                    .child(
                        Icon::new(if list.is_some() { Lucide::ChevronUp } else { Lucide::ChevronDown })
                            .with_size(px(14.)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(panel) = this.pull_requests.panel.as_mut() {
                            panel.guide_rail_open = !panel.guide_rail_open;
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(
                        xs_outline("pr-guide-prev", t)
                            .size(px(24.))
                            .p_0()
                            .tooltip("Previous stop")
                            .disabled(index == 0)
                            .child(Icon::new(Lucide::ChevronUp).with_size(px(14.)))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_pr_guide_stop(index.saturating_sub(1), cx)),
                            ),
                    )
                    .child(
                        xs_outline("pr-guide-next", t)
                            .size(px(24.))
                            .p_0()
                            .tooltip("Next stop")
                            .disabled(index + 1 >= count)
                            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_pr_guide_stop((index + 1).min(count - 1), cx);
                            })),
                    ),
            );
        let summary = (!stop.summary.trim().is_empty())
            .then(|| self.pr_markdown(&format!("guide:{index}"), &stop.summary, cx))
            .flatten()
            .map(|state| {
                div().text_sm().text_color(t.muted_foreground).child(pr_markdown_view(&state, t, cx.entity()))
            });
        let attention = stop.attention.as_ref().map(|a| {
            h_flex()
                .items_start()
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .px(px(12.))
                .py(px(10.))
                .child(div().mt(px(2.)).child(Icon::new(Lucide::Eye).with_size(px(16.))))
                .child(
                    v_flex()
                        .min_w_0()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.foreground)
                                .child(logic::attention_label(&a.kind)),
                        )
                        .child(div().text_sm().text_color(t.muted_foreground).child(a.note.clone())),
                )
        });
        let notes_by_excerpt = logic::excerpt_note_lines(stop);
        let loading = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()).is_none_or(|c| !c.loaded());
        let excerpts: Vec<AnyElement> = stop
            .excerpts
            .iter()
            .enumerate()
            .map(|(i, excerpt)| self.render_pr_guide_excerpt(excerpt, &notes_by_excerpt[i], loading, t, cx))
            .collect();
        let notes = (!stop.notes.is_empty()).then(|| {
            v_flex()
                .gap(px(8.))
                .child(
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .child(
                            div().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child("Notes on this stop"),
                        )
                        .child(div().text_color(t.muted_foreground).child(if stop.notes.len() == 1 {
                            "1 note".to_owned()
                        } else {
                            format!("{} notes", stop.notes.len())
                        })),
                )
                .children(stop.notes.iter().enumerate().map(|(i, note)| {
                    let file = note.path.rsplit('/').next().unwrap_or(&note.path);
                    h_flex().items_start().gap(px(8.)).child(marker_chip(&logic::note_marker(i), t)).child(
                        v_flex()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                h_flex()
                                    .flex_wrap()
                                    .gap(px(8.))
                                    .text_xs()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t.foreground)
                                            .child(note.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .font_family(FONT_MONO)
                                            .text_size(px(11.))
                                            .text_color(t.muted_foreground)
                                            .child(format!("{file}:{}", note.line)),
                                    ),
                            )
                            .when(!note.body.trim().is_empty(), |el| {
                                el.child(div().text_xs().text_color(t.muted_foreground).child(note.body.clone()))
                            }),
                    )
                }))
        });
        let excerpted: HashSet<&str> = stop.excerpts.iter().map(|e| e.path.as_str()).collect();
        let others: Vec<(String, Option<String>)> = stop
            .files
            .iter()
            .filter(|f| !excerpted.contains(f.path.as_str()))
            .map(|f| (f.path.clone(), f.note.clone()))
            .collect();
        let files = (!others.is_empty())
            .then(|| self.render_pr_guide_files(&others, !stop.excerpts.is_empty(), loading, t, cx));
        let ask = self.render_pr_guide_questions(stop, index, t, cx);
        let footer = h_flex()
            .justify_end()
            .pt(px(16.))
            .border_t_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .child(match next {
                Some(title) => Button::new("pr-guide-continue")
                    .primary()
                    .small()
                    .child(div().min_w_0().truncate().child(format!("Next: {title}")))
                    .child(Icon::new(Lucide::ArrowRight).with_size(px(14.)))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_pr_guide_stop(index + 1, cx)))
                    .into_any_element(),
                None => Button::new("pr-guide-finish")
                    .primary()
                    .small()
                    .child(Icon::new(Lucide::MessageSquare).with_size(px(14.)))
                    .child("Finish review")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(panel) = this.pull_requests.panel.as_mut() {
                            panel.guide_seen.insert(index);
                        }
                        this.open_pr_review(window, cx);
                    }))
                    .into_any_element(),
            });
        v_flex()
            .min_w_0()
            .gap(px(20.))
            .child(v_flex().gap(px(12.)).child(header).children(list))
            .child(
                v_flex()
                    .max_w(px(960.))
                    .gap(px(8.))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(stop.title.clone()),
                    )
                    .children(summary),
            )
            .children(attention)
            .when(!excerpts.is_empty(), |el| el.child(v_flex().gap(px(16.)).children(excerpts)))
            .children(notes)
            .children(files)
            .children(ask)
            .child(footer)
            .into_any_element()
    }

    /// `GuideExcerpt`: the lines the guide points at, with its notes' markers under theirs.
    fn render_pr_guide_excerpt(
        &self,
        excerpt: &GuideExcerpt,
        notes: &[(u64, Vec<(String, String)>)],
        loading: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let new_side = excerpt.side != "old";
        let rows = self
            .pull_requests
            .panel
            .as_ref()
            .and_then(|p| p.code.as_ref())
            .and_then(|c| c.patch(&excerpt.path))
            .map(|file| diff_logic::excerpt_rows(&diff_logic::file_rows(file), new_side, excerpt.start, excerpt.end))
            .unwrap_or_default();
        let caption = (!excerpt.caption.trim().is_empty())
            .then(|| div().text_xs().text_color(t.muted_foreground).child(excerpt.caption.clone()));
        let path = excerpt.path.clone();
        let heading = h_flex()
            .justify_between()
            .gap(px(12.))
            .px(px(12.))
            .py(px(6.))
            .border_b_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .text_xs()
            .text_color(t.muted_foreground)
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(6.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(FONT_MONO)
                            .text_color(t.foreground)
                            .child(excerpt.path.clone()),
                    )
                    .child(div().flex_none().child(format!("lines {}-{}", excerpt.start, excerpt.end))),
            )
            .child(
                ghost_button(ElementId::Name(format!("pr-guide-open:{}:{}", excerpt.path, excerpt.start).into()), t)
                    .child("Open file")
                    .on_click(cx.listener(move |this, _, _, cx| this.open_pr_code_file(path.clone(), cx))),
            );
        let body = if rows.is_empty() {
            div()
                .px(px(12.))
                .py(px(8.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(if loading { "Loading the diff..." } else { "These lines are not in the loaded diff." })
                .into_any_element()
        } else {
            code_rows(&rows, notes, new_side, t)
        };
        v_flex()
            .min_w_0()
            .gap(px(6.))
            .children(caption)
            .child(
                v_flex()
                    .min_w_0()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .child(heading)
                    .child(body),
            )
            .into_any_element()
    }

    /// "Also in this stop": the stop's other files, each opening to its diff.
    fn render_pr_guide_files(
        &self,
        files: &[(String, Option<String>)],
        after_excerpts: bool,
        loading: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let code = panel.code.as_ref();
        let rows: Vec<AnyElement> = files
            .iter()
            .enumerate()
            .map(|(i, (path, note))| {
                let open = panel.guide_files_open.contains(path);
                let patch = code.and_then(|c| c.patch(path));
                let stat = patch.map(diff_logic::FilePatch::stat);
                let key = path.clone();
                let trigger = h_flex()
                    .id(ElementId::Name(format!("pr-guide-file:{path}").into()))
                    .w_full()
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(8.))
                    .text_xs()
                    .cursor_pointer()
                    .hover(|s| s.bg(Hsla { a: t.accent.a * 0.5, ..t.accent }))
                    .child(
                        Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight })
                            .with_size(px(14.))
                            .text_color(t.muted_foreground),
                    )
                    .child(Icon::new(Lucide::File).with_size(px(14.)).text_color(t.muted_foreground))
                    .child(
                        div().min_w_0().truncate().font_family(FONT_MONO).text_color(t.foreground).child(path.clone()),
                    )
                    .children(
                        note.clone()
                            .filter(|n| !n.trim().is_empty())
                            .map(|n| div().ml_auto().min_w_0().truncate().text_color(t.muted_foreground).child(n)),
                    )
                    .children(stat.and_then(|(a, d)| diff_stat(a, d, 11., t)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(panel) = this.pull_requests.panel.as_mut()
                            && !panel.guide_files_open.remove(&key)
                        {
                            panel.guide_files_open.insert(key.clone());
                        }
                        cx.notify();
                    }));
                let body = open.then(|| match patch {
                    Some(patch) => code_rows(&diff_logic::file_rows(patch), &[], true, t),
                    None => div()
                        .px(px(12.))
                        .py(px(8.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(if loading { "Loading the diff..." } else { "This file is not in the loaded diff." })
                        .into_any_element(),
                });
                v_flex()
                    .when(i > 0, |el| el.border_t_1().border_color(Hsla { a: t.border.a * 0.6, ..t.border }))
                    .child(trigger)
                    .children(body)
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap(px(6.))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .child(if after_excerpts { "Also in this stop" } else { "Files in this stop" }),
            )
            .child(
                v_flex()
                    .overflow_hidden()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .children(rows),
            )
            .into_any_element()
    }

    /// "Questions to consider", each one a question to hand an agent, and "Ask about this stop".
    fn render_pr_guide_questions(
        &self,
        stop: &GuideStop,
        index: usize,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let can_ask = self
            .pull_requests
            .panel
            .as_ref()
            .and_then(|p| p.detail.as_ref())
            .is_some_and(|d| self.shell.projects.iter().any(|p| p.id == d.project_id));
        if stop.questions.is_empty() && !can_ask {
            return None;
        }
        let questions: Vec<AnyElement> = stop
            .questions
            .iter()
            .enumerate()
            .map(|(i, question)| {
                if !can_ask {
                    return div().text_sm().text_color(t.foreground).child(question.clone()).into_any_element();
                }
                let asked = question.clone();
                h_flex()
                    .id(ElementId::Name(format!("pr-guide-q-{index}-{i}").into()))
                    .items_start()
                    .gap(px(8.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .text_sm()
                    .text_color(t.foreground)
                    .cursor_pointer()
                    .hover(|s| s.bg(Hsla { a: t.accent.a * 0.5, ..t.accent }))
                    .child(div().mt(px(3.)).child(
                        Icon::new(Lucide::MessageCircleQuestionMark).with_size(px(14.)).text_color(t.muted_foreground),
                    ))
                    .child(question.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_about_pr_guide_stop(index, asked.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        Some(
            v_flex()
                .items_start()
                .gap(px(6.))
                .when(!stop.questions.is_empty(), |el| {
                    el.child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.muted_foreground)
                            .child("Questions to consider"),
                    )
                })
                .children(questions)
                .when(can_ask, |el| {
                    el.child(
                        ghost_button("pr-guide-ask", t)
                            .child(Icon::new(Lucide::MessageCircleQuestionMark).with_size(px(14.)))
                            .child("Ask about this stop")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask_about_pr_guide_stop(index, String::new(), window, cx);
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    fn ask_about_pr_guide_stop(&mut self, index: usize, question: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let Some(stop) = panel.guide_tour.as_ref().and_then(|g| g.as_ref().ok()?.stops.get(index).cloned()) else {
            return;
        };
        let prompt = logic::guide_stop_handoff(&logic::HandoffSubject::of(&detail), index, &stop, &question);
        self.start_pr_ask(prompt, window, cx);
    }

    /// "Open file": the Code tab, scrolled to the file.
    fn open_pr_code_file(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(panel) = self.pull_requests.panel.as_mut() {
            panel.tab = Tab::Code;
        }
        self.open_pr_code(cx);
        if let Some(code) = self.pull_requests.panel.as_ref().and_then(|p| p.code.as_ref()) {
            code.reveal(&path, cx);
        }
        cx.notify();
    }
}

/// `GuideNoteMarker`.
fn marker_chip(marker: &str, t: &Tokens) -> impl IntoElement {
    div()
        .size(px(18.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(Hsla { a: 0.16, ..t.info })
        .text_size(px(9.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.info_foreground)
        .child(marker.to_owned())
}

/// A few lines of a diff, numbered on both sides, with note markers under the lines they name.
fn code_rows(rows: &[Row], notes: &[(u64, Vec<(String, String)>)], new_side: bool, t: &Tokens) -> AnyElement {
    let added = if t.dark { gpui_kit::rgb(0x00d492).into() } else { gpui_kit::rgb(0x007a55).into() };
    let tint = |color: Hsla| Hsla { a: 0.12, ..color };
    let number = |n: Option<u64>| {
        div()
            .w(px(40.))
            .flex_none()
            .pr(px(8.))
            .text_right()
            .text_color(Hsla { a: 0.6, ..t.muted_foreground })
            .child(n.map(|n| n.to_string()).unwrap_or_default())
    };
    let mut out = v_flex().font_family(FONT_MONO).text_size(px(12.)).line_height(px(20.)).py(px(4.));
    for row in rows {
        let line = if new_side { row.new } else { row.old };
        let bg = match row.mark {
            '+' => Some(tint(added)),
            '-' => Some(tint(t.error)),
            _ => None,
        };
        out = out.child(
            h_flex()
                .min_w_0()
                .when_some(bg, gpui_kit::Styled::bg)
                .child(number(row.old))
                .child(number(row.new))
                .child(div().w(px(16.)).flex_none().text_color(t.muted_foreground).child(row.mark.to_string()))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_color(t.foreground)
                        .child(row.text.to_owned()),
                ),
        );
        if let Some((_, markers)) = line.and_then(|l| notes.iter().find(|(at, _)| *at == l)) {
            out = out.child(
                h_flex()
                    .flex_wrap()
                    .gap(px(12.))
                    .px(px(12.))
                    .py(px(4.))
                    .font_family(theme::FONT_SANS)
                    .text_xs()
                    .children(markers.iter().map(|(marker, title)| {
                        h_flex()
                            .gap(px(6.))
                            .child(marker_chip(marker, t))
                            .child(div().text_color(t.foreground).child(title.clone()))
                    })),
            );
        }
    }
    out.into_any_element()
}
