pub mod playable_character;
pub mod playable_dynamics;
pub mod street_city;

use yuyib::{
    render::{RenderFrame, wgpu},
    ui::{
        Color, ColorToken, Dimension, LayoutConstraints, LayoutKind, Point, Size, UiBuilder,
        UiTokens, Widget, WidgetId, WidgetStyle, layout,
    },
    ui_render::{UiRenderLimits, UiRenderer},
};

/// Small reusable loading overlay for the interactive examples.
#[derive(Default)]
pub struct LoadingScreen {
    renderer: Option<UiRenderer>,
}

impl LoadingScreen {
    pub fn draw(&mut self, frame: &mut RenderFrame<'_>, fraction: f32, failed: bool) {
        frame.with_surface_pass(
            wgpu::LoadOp::Clear(wgpu::Color {
                r: 0.008,
                g: 0.012,
                b: 0.025,
                a: 1.0,
            }),
            |_| {},
        );
        let [width, height] = frame.surface_size();
        let tree = loading_tree(width, height, fraction, failed);
        let Ok(layout) = layout(&tree, Size::new(width, height)) else {
            return;
        };
        let renderer = self
            .renderer
            .get_or_insert_with(|| UiRenderer::new_for_frame(frame));
        let _ = renderer.draw(
            frame,
            tree.root(),
            &layout,
            UiTokens::default(),
            UiRenderLimits::default(),
        );
    }
}

fn loading_tree(width: u32, height: u32, fraction: f32, failed: bool) -> yuyib::ui::UiTree {
    let bar_width = width.saturating_sub(96).clamp(260, 720);
    let bar_height = 22;
    let left = i32::try_from(width.saturating_sub(bar_width) / 2).unwrap_or(i32::MAX);
    let top = i32::try_from(height / 2).unwrap_or(i32::MAX);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "fraction is clamped before conversion to bounded thousandths"
    )]
    let filled = (fraction.clamp(0.0, 1.0) * 1_000.0).round() as u16;
    let filled_width = bar_width.saturating_mul(u32::from(filled)) / 1_000;
    let absolute = |width, height, left, top| {
        LayoutConstraints::auto()
            .with_width(Dimension::Points(width))
            .with_height(Dimension::Points(height))
            .with_absolute_position(Point::new(left, top))
    };
    let colour = if failed {
        Color::rgb(210, 83, 83)
    } else {
        Color::rgb(84, 168, 255)
    };
    UiBuilder::new(WidgetId::from_key("loading-root"), LayoutKind::Absolute)
        .child(
            Widget::container(WidgetId::from_key("background"), LayoutKind::Absolute)
                .with_constraints(absolute(width, height, 0, 0))
                .with_style(
                    WidgetStyle::default()
                        .with_background(ColorToken::Custom(Color::rgb(4, 7, 16))),
                ),
        )
        .child(
            Widget::container(WidgetId::from_key("track"), LayoutKind::Absolute)
                .with_constraints(absolute(bar_width, bar_height, left, top))
                .with_style(
                    WidgetStyle::default()
                        .with_background(ColorToken::Custom(Color::rgb(45, 56, 82))),
                ),
        )
        .child(
            Widget::container(WidgetId::from_key("fill"), LayoutKind::Absolute)
                .with_constraints(absolute(filled_width, bar_height, left, top))
                .with_style(WidgetStyle::default().with_background(ColorToken::Custom(colour))),
        )
        .build()
        .expect("constant loading UI IDs are unique")
}
