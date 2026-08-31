use crate::config::DisplayFormat;
use crate::render::RenderConfig;
use crate::selected_frame::SelectableFrame;
use crate::widgets::komorebi::KomorebiLayoutConfig;
use color_eyre::eyre;
use eframe::egui::Color32;
use eframe::egui::Context;
use eframe::egui::CornerRadius;
use eframe::egui::FontId;
use eframe::egui::Frame;
use eframe::egui::Label;
use eframe::egui::Rect;
use eframe::egui::Sense;
use eframe::egui::Stroke;
use eframe::egui::StrokeKind;
use eframe::egui::Ui;
use eframe::egui::Vec2;
use eframe::egui::vec2;
use komorebi_client::SocketMessage;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::de::Error;
use serde_json::from_str;
use std::fmt::Display;
use std::fmt::Formatter;

#[derive(Copy, Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
/// Komorebi layout kind
pub enum KomorebiLayout {
    /// Predefined layout
    #[cfg_attr(feature = "schemars", schemars(title = "Default"))]
    Default(komorebi_client::DefaultLayout),
    /// Monocle mode
    Monocle,
    /// Floating layer
    Floating,
    /// Paused
    Paused,
    /// Custom layout
    Custom,
}

impl<'de> Deserialize<'de> for KomorebiLayout {
    fn deserialize<D>(deserializer: D) -> eyre::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: String = String::deserialize(deserializer)?;

        // Attempt to deserialize the string as a DefaultLayout
        if let Ok(default_layout) = from_str::<komorebi_client::DefaultLayout>(&format!("\"{s}\""))
        {
            return Ok(KomorebiLayout::Default(default_layout));
        }

        // Handle other cases
        match s.as_str() {
            "Monocle" => Ok(KomorebiLayout::Monocle),
            "Floating" => Ok(KomorebiLayout::Floating),
            "Paused" => Ok(KomorebiLayout::Paused),
            "Custom" => Ok(KomorebiLayout::Custom),
            _ => Err(Error::custom(format!("Invalid layout: {s}"))),
        }
    }
}

impl Display for KomorebiLayout {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            KomorebiLayout::Default(layout) => write!(f, "{layout}"),
            KomorebiLayout::Monocle => write!(f, "Monocle"),
            KomorebiLayout::Floating => write!(f, "Floating"),
            KomorebiLayout::Paused => write!(f, "Paused"),
            KomorebiLayout::Custom => write!(f, "Custom"),
        }
    }
}

/// One active container's slot, as a fraction of the work area.
///
/// Normalized rather than kept in pixels because the icon it is drawn into is a dozen pixels across
/// and knows nothing about the monitor: the arrangement is a shape, and only its proportions
/// survive the trip.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct ArrangementSlot {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    pub is_focused: bool,
}

/// The active logical slots of a workspace, in top-to-bottom order.
///
/// This is what makes the layout icon say something about *this* workspace rather than about the
/// layout kind it was tiled with: the same BSP layout draws one cell with one container and four
/// with four, and a manual split or a hidden container changes the icon the moment it changes the
/// desktop. Hidden containers hold no slot and so are not drawn, which is the same rule the tiling
/// itself follows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KomorebiArrangement {
    pub slots: Vec<ArrangementSlot>,
}

impl KomorebiArrangement {
    /// Read the arrangement off a workspace, normalizing every slot into the unit square.
    ///
    /// The frame is the work area the slots were calculated against. A workspace which has not been
    /// arranged yet has no recorded work area, and the slots' own bounding box stands in for it -
    /// active slots cover the work area exactly, so the two agree whenever both exist.
    pub fn from_workspace(workspace: &komorebi_client::Workspace) -> Self {
        let slots = workspace
            .logical_slots
            .ordered(komorebi_client::SlotOrder::TopToBottom);

        let Some(frame) = workspace.logical_work_area.or_else(|| Self::bounds(&slots)) else {
            return Self::default();
        };

        if frame.width <= 0 || frame.height <= 0 {
            return Self::default();
        }

        let focused = workspace
            .focused_container()
            .map(|container| container.id.clone());

        let width = frame.width as f32;
        let height = frame.height as f32;

        Self {
            slots: slots
                .into_iter()
                .map(|(id, slot)| ArrangementSlot {
                    left: (slot.left - frame.left) as f32 / width,
                    top: (slot.top - frame.top) as f32 / height,
                    width: slot.width as f32 / width,
                    height: slot.height as f32 / height,
                    is_focused: focused.as_ref() == Some(&id),
                })
                .collect(),
        }
    }

    fn bounds(
        slots: &[(komorebi_client::ContainerId, komorebi_client::LogicalRect)],
    ) -> Option<komorebi_client::LogicalRect> {
        let (first, rest) = slots.split_first()?;
        let mut left = first.1.left;
        let mut top = first.1.top;
        let mut right = first.1.right();
        let mut bottom = first.1.bottom();

        for (_, slot) in rest {
            left = left.min(slot.left);
            top = top.min(slot.top);
            right = right.max(slot.right());
            bottom = bottom.max(slot.bottom());
        }

        Some(komorebi_client::LogicalRect::new(
            left,
            top,
            right - left,
            bottom - top,
        ))
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

impl KomorebiLayout {
    fn is_default(&mut self) -> bool {
        matches!(self, KomorebiLayout::Default(_))
    }

    fn on_click(
        &mut self,
        show_options: &bool,
        monitor_idx: usize,
        workspace_idx: Option<usize>,
    ) -> bool {
        if self.is_default() {
            !show_options
        } else {
            self.on_click_option(monitor_idx, workspace_idx);
            false
        }
    }

    fn on_click_option(&mut self, monitor_idx: usize, workspace_idx: Option<usize>) {
        match self {
            KomorebiLayout::Default(option) => {
                if let Some(ws_idx) = workspace_idx
                    && komorebi_client::send_message(&SocketMessage::WorkspaceLayout(
                        monitor_idx,
                        ws_idx,
                        *option,
                    ))
                    .is_err()
                {
                    tracing::error!("could not send message to komorebi: WorkspaceLayout");
                }
            }
            KomorebiLayout::Monocle => {
                if komorebi_client::send_batch([
                    SocketMessage::FocusMonitorAtCursor,
                    SocketMessage::ToggleMonocle,
                ])
                .is_err()
                {
                    tracing::error!("could not send message to komorebi: ToggleMonocle");
                }
            }
            KomorebiLayout::Floating => {
                if komorebi_client::send_batch([
                    SocketMessage::FocusMonitorAtCursor,
                    SocketMessage::ToggleTiling,
                ])
                .is_err()
                {
                    tracing::error!("could not send message to komorebi: ToggleTiling");
                }
            }
            KomorebiLayout::Paused => {
                if komorebi_client::send_message(&SocketMessage::TogglePause).is_err() {
                    tracing::error!("could not send message to komorebi: TogglePause");
                }
            }
            KomorebiLayout::Custom => {}
        }
    }

    /// Paint the arrangement the workspace actually holds: one cell per active container.
    ///
    /// Only the interior edges are drawn, because the outer frame is the icon's own border - a cell
    /// contributes its left edge unless that edge is the frame's, and its top edge unless that edge
    /// is the frame's, so every dividing line is drawn exactly once whatever the arrangement. The
    /// focused container's cell is filled faintly, which is the one piece of state a count and a
    /// set of proportions cannot carry.
    fn show_arrangement_icon(
        arrangement: &KomorebiArrangement,
        is_selected: bool,
        font_id: FontId,
        ctx: &Context,
        ui: &mut Ui,
    ) {
        let size = Vec2::splat(font_id.size);
        let (response, painter) = ui.allocate_painter(size, Sense::hover());
        let color = if is_selected {
            ctx.style().visuals.selection.stroke.color
        } else {
            ui.style().visuals.text_color()
        };
        let stroke = Stroke::new(1.0_f32, color);
        let mut rect = response.rect;
        let rounding = CornerRadius::same((rect.width() * 0.1) as u8);
        rect = rect.shrink(stroke.width);
        painter.rect_stroke(rect, rounding, stroke, StrokeKind::Outside);

        let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 80);
        // A cell edge lands on the frame edge when its fraction rounds to a whole side; the
        // tolerance is in fractions of the work area, so half of the thinnest cell a monitor can
        // hold is still well above it.
        let epsilon = 0.01_f32;

        for slot in &arrangement.slots {
            let min = rect.min + vec2(slot.left * rect.width(), slot.top * rect.height());
            let cell = Rect::from_min_size(
                min,
                vec2(slot.width * rect.width(), slot.height * rect.height()),
            );

            if slot.is_focused {
                painter.rect_filled(cell, CornerRadius::ZERO, fill);
            }

            if slot.left > epsilon {
                painter.line_segment([cell.left_top(), cell.left_bottom()], stroke);
            }

            if slot.top > epsilon {
                painter.line_segment([cell.left_top(), cell.right_top()], stroke);
            }
        }
    }

    fn show_icon(&mut self, is_selected: bool, font_id: FontId, ctx: &Context, ui: &mut Ui) {
        // paint custom icons for the layout
        let size = Vec2::splat(font_id.size);
        let (response, painter) = ui.allocate_painter(size, Sense::hover());
        let color = if is_selected {
            ctx.style().visuals.selection.stroke.color
        } else {
            ui.style().visuals.text_color()
        };
        let stroke = Stroke::new(1.0_f32, color);
        let mut rect = response.rect;
        let rounding = CornerRadius::same((rect.width() * 0.1) as u8);
        rect = rect.shrink(stroke.width);
        let c = rect.center();
        let r = rect.width() / 2.0;
        painter.rect_stroke(rect, rounding, stroke, StrokeKind::Outside);

        match self {
            KomorebiLayout::Default(layout) => match layout {
                komorebi_client::DefaultLayout::BSP => {
                    painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                    painter.line_segment([c, c + vec2(r, 0.0)], stroke);
                    painter.line_segment([c + vec2(r / 2.0, 0.0), c + vec2(r / 2.0, r)], stroke);
                }
                komorebi_client::DefaultLayout::Columns => {
                    painter.line_segment([c - vec2(r / 2.0, r), c + vec2(-r / 2.0, r)], stroke);
                    painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                    painter.line_segment([c - vec2(-r / 2.0, r), c + vec2(r / 2.0, r)], stroke);
                }
                komorebi_client::DefaultLayout::Rows => {
                    painter.line_segment([c - vec2(r, r / 2.0), c + vec2(r, -r / 2.0)], stroke);
                    painter.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], stroke);
                    painter.line_segment([c - vec2(r, -r / 2.0), c + vec2(r, r / 2.0)], stroke);
                }
                komorebi_client::DefaultLayout::VerticalStack => {
                    painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                    painter.line_segment([c, c + vec2(r, 0.0)], stroke);
                }
                komorebi_client::DefaultLayout::RightMainVerticalStack => {
                    painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                    painter.line_segment([c - vec2(r, 0.0), c], stroke);
                }
                komorebi_client::DefaultLayout::HorizontalStack => {
                    painter.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], stroke);
                    painter.line_segment([c, c + vec2(0.0, r)], stroke);
                }
                komorebi_client::DefaultLayout::UltrawideVerticalStack => {
                    painter.line_segment([c - vec2(r / 2.0, r), c + vec2(-r / 2.0, r)], stroke);
                    painter.line_segment([c + vec2(r / 2.0, 0.0), c + vec2(r, 0.0)], stroke);
                    painter.line_segment([c - vec2(-r / 2.0, r), c + vec2(r / 2.0, r)], stroke);
                }
                komorebi_client::DefaultLayout::Grid => {
                    painter.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], stroke);
                    painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                }
                // TODO: @CtByte can you think of a nice icon to draw here?
                komorebi_client::DefaultLayout::Scrolling => {
                    painter.line_segment([c - vec2(r / 2.0, r), c + vec2(-r / 2.0, r)], stroke);
                    painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                    painter.line_segment([c - vec2(-r / 2.0, r), c + vec2(r / 2.0, r)], stroke);
                }
            },
            KomorebiLayout::Monocle => {}
            KomorebiLayout::Floating => {
                let mut rect_left = response.rect;
                rect_left.set_width(rect.width() * 0.5);
                rect_left.set_height(rect.height() * 0.5);
                let mut rect_right = rect_left;
                rect_left = rect_left.translate(Vec2::new(
                    rect.width() * 0.1 + stroke.width,
                    rect.width() * 0.1 + stroke.width,
                ));
                rect_right = rect_right.translate(Vec2::new(
                    rect.width() * 0.35 + stroke.width,
                    rect.width() * 0.35 + stroke.width,
                ));
                painter.rect_filled(rect_left, rounding, color);
                painter.rect_stroke(rect_right, rounding, stroke, StrokeKind::Outside);
            }
            KomorebiLayout::Paused => {
                let mut rect_left = response.rect;
                rect_left.set_width(rect.width() * 0.25);
                rect_left.set_height(rect.height() * 0.8);
                let mut rect_right = rect_left;
                rect_left = rect_left.translate(Vec2::new(
                    rect.width() * 0.2 + stroke.width,
                    rect.width() * 0.1 + stroke.width,
                ));
                rect_right = rect_right.translate(Vec2::new(
                    rect.width() * 0.55 + stroke.width,
                    rect.width() * 0.1 + stroke.width,
                ));
                painter.rect_filled(rect_left, rounding, color);
                painter.rect_filled(rect_right, rounding, color);
            }
            KomorebiLayout::Custom => {
                painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
                painter.line_segment([c + vec2(0.0, r / 2.0), c + vec2(r, r / 2.0)], stroke);
                painter.line_segment([c - vec2(0.0, r / 3.0), c - vec2(r, r / 3.0)], stroke);
            }
        }
    }

    /// Whether this layout is one whose icon may be replaced by the workspace's own arrangement.
    ///
    /// Monocle, the floating layer and a pause are modes rather than arrangements: their icons say
    /// which mode is on, which is not something a set of slots can express, and a monocle in
    /// particular is showing one container over an arrangement that still exists underneath it.
    const fn draws_an_arrangement(&self) -> bool {
        matches!(self, Self::Default(_) | Self::Custom)
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        ui: &mut Ui,
        render_config: &mut RenderConfig,
        layout_config: &KomorebiLayoutConfig,
        workspace_idx: Option<usize>,
        arrangement: &KomorebiArrangement,
    ) {
        let monitor_idx = render_config.monitor_idx;
        let font_id = render_config.icon_font_id.clone();
        let mut show_options = RenderConfig::load_show_komorebi_layout_options();
        let format = layout_config.display.unwrap_or(DisplayFormat::IconAndText);

        if !self.is_default() {
            show_options = false;
        }

        let draw_arrangement = self.draws_an_arrangement() && !arrangement.is_empty();
        let hover_text = if draw_arrangement {
            match arrangement.slots.len() {
                1 => format!("{self}, 1 container"),
                count => format!("{self}, {count} containers"),
            }
        } else {
            self.to_string()
        };

        render_config.apply_on_widget(false, ui, |ui| {
            let layout_frame = SelectableFrame::new(false)
                .show(ui, |ui| {
                    if let DisplayFormat::Icon | DisplayFormat::IconAndText = format {
                        if draw_arrangement {
                            Self::show_arrangement_icon(
                                arrangement,
                                true,
                                font_id.clone(),
                                ctx,
                                ui,
                            );
                        } else {
                            self.show_icon(true, font_id.clone(), ctx, ui);
                        }
                    }

                    if let DisplayFormat::Text | DisplayFormat::IconAndText = format {
                        ui.add(Label::new(self.to_string()).selectable(false));
                    }
                })
                .on_hover_text(&hover_text);

            if layout_frame.clicked() {
                show_options = self.on_click(&show_options, monitor_idx, workspace_idx);
            }

            if show_options && let Some(workspace_idx) = workspace_idx {
                Frame::NONE.show(ui, |ui| {
                    ui.add(
                        Label::new(egui_phosphor::regular::ARROW_FAT_LINES_RIGHT.to_string())
                            .selectable(false),
                    );

                    let mut layout_options = layout_config.options.clone().unwrap_or(vec![
                        KomorebiLayout::Default(komorebi_client::DefaultLayout::BSP),
                        KomorebiLayout::Default(komorebi_client::DefaultLayout::Columns),
                        KomorebiLayout::Default(komorebi_client::DefaultLayout::Rows),
                        KomorebiLayout::Default(komorebi_client::DefaultLayout::VerticalStack),
                        KomorebiLayout::Default(
                            komorebi_client::DefaultLayout::RightMainVerticalStack,
                        ),
                        KomorebiLayout::Default(komorebi_client::DefaultLayout::HorizontalStack),
                        KomorebiLayout::Default(
                            komorebi_client::DefaultLayout::UltrawideVerticalStack,
                        ),
                        KomorebiLayout::Default(komorebi_client::DefaultLayout::Grid),
                        //KomorebiLayout::Custom,
                        KomorebiLayout::Monocle,
                        KomorebiLayout::Floating,
                        KomorebiLayout::Paused,
                    ]);

                    for layout_option in &mut layout_options {
                        let is_selected = self == layout_option;

                        if SelectableFrame::new(is_selected)
                            .show(ui, |ui| {
                                layout_option.show_icon(is_selected, font_id.clone(), ctx, ui)
                            })
                            .on_hover_text(match layout_option {
                                KomorebiLayout::Default(layout) => layout.to_string(),
                                KomorebiLayout::Monocle => "Toggle monocle".to_string(),
                                KomorebiLayout::Floating => "Toggle tiling".to_string(),
                                KomorebiLayout::Paused => "Toggle pause".to_string(),
                                KomorebiLayout::Custom => "Custom".to_string(),
                            })
                            .clicked()
                        {
                            layout_option.on_click_option(monitor_idx, Some(workspace_idx));
                            show_options = false;
                        };
                    }
                });
            }
        });

        RenderConfig::store_show_komorebi_layout_options(show_options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komorebi_client::ContainerId;
    use komorebi_client::LogicalRect;
    use komorebi_client::Workspace;

    fn workspace(slots: &[(&str, LogicalRect)]) -> Workspace {
        let mut workspace = Workspace::default();

        for (id, slot) in slots {
            workspace.logical_slots.set(ContainerId::from(*id), *slot);
        }

        workspace
    }

    #[test]
    fn a_workspace_with_no_slots_draws_nothing() {
        assert!(KomorebiArrangement::from_workspace(&Workspace::default()).is_empty());
    }

    #[test]
    fn one_container_fills_the_icon() {
        let workspace = workspace(&[("only", LogicalRect::new(0, 0, 1920, 1080))]);

        assert_eq!(
            KomorebiArrangement::from_workspace(&workspace).slots,
            vec![ArrangementSlot {
                left: 0.0,
                top: 0.0,
                width: 1.0,
                height: 1.0,
                is_focused: false,
            }]
        );
    }

    #[test]
    fn a_quartered_work_area_normalizes_to_four_quarters_top_to_bottom() {
        // The work area does not start at the origin, so an arrangement which ignored the frame's
        // own offset would push every cell out of the icon.
        let workspace = workspace(&[
            ("top left", LogicalRect::new(100, 40, 960, 520)),
            ("bottom left", LogicalRect::new(100, 560, 960, 520)),
            ("top right", LogicalRect::new(1060, 40, 960, 520)),
            ("bottom right", LogicalRect::new(1060, 560, 960, 520)),
        ]);

        let slots = KomorebiArrangement::from_workspace(&workspace).slots;

        assert_eq!(slots.len(), 4);
        assert_eq!(
            (slots[0].left, slots[0].top, slots[0].width, slots[0].height),
            (0.0, 0.0, 0.5, 0.5)
        );
        assert_eq!(
            (slots[1].left, slots[1].top, slots[1].width, slots[1].height),
            (0.5, 0.0, 0.5, 0.5)
        );
        // Top to bottom, then left to right, so the two bottom cells come last.
        assert_eq!((slots[2].left, slots[2].top), (0.0, 0.5));
        assert_eq!((slots[3].left, slots[3].top), (0.5, 0.5));
    }

    #[test]
    fn the_recorded_work_area_is_the_frame_when_there_is_one() {
        // A work area taller than the slots cover cannot happen in a tiled workspace, but it says
        // which of the two frames is being used.
        let mut workspace = workspace(&[("only", LogicalRect::new(0, 0, 1920, 540))]);
        workspace.logical_work_area = Some(LogicalRect::new(0, 0, 1920, 1080));

        let slots = KomorebiArrangement::from_workspace(&workspace).slots;

        assert_eq!(slots[0].height, 0.5);
    }
}
