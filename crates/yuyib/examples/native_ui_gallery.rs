//! Галерея реально доступных нативных UI-элементов.
//!
//! Запуск из корня workspace:
//!
//! ```text
//! cargo run -p yuyib --example native_ui_gallery
//! ```
//!
//! Это не макет HTML и не отдельный UI-язык. Пример показывает готовый
//! высокоуровневый путь `ApplicationUi`: дерево из Rust-виджетов, один явно
//! заданный шрифт, `with_visual_style`, `Widget::image` / `UiImageId`,
//! автоматический размер текста и ввод Winit. Нажатия мышью, `Tab`,
//! `Enter`/`Space`, колесо и **drag полосы прокрутки** печатаются в терминал.
//! Для низкоуровневой работы остаются `UiTree`, `layout_with_measurer`,
//! `UiRenderer`, `extract_draw_list` / `extract_image_draw_list` и text-проходы.
//!
//! Левая колонка — `ScrollView`: колесо или перетаскивание thumb / клик по track.

use std::error::Error;

use yuyib::{
    app::{
        Application, ApplicationUi, DialogueOverlayContent, NativeUiTextConfig, RenderLoop,
        UiVisualStyle, dialogue_overlay_tree,
    },
    input::{UiDpiPolicy, WinitUiAdapter},
    platform::WindowConfig,
    render::ClearColor,
    ui::{
        Color, ColorToken, Dimension, Insets, LayoutConstraints, LayoutKind, Size, UiBuilder,
        UiImageId, Widget, WidgetId, WidgetStyle,
    },
    ui_text::FontSource,
};

const DEV_FONT_FILE: &str = r"C:\Windows\Fonts\segoeui.ttf";

fn main() -> Result<(), Box<dyn Error>> {
    let input = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels)?;
    let visuals = UiVisualStyle::default().with_scroll_thumb(Some(Color::rgba(210, 220, 240, 200)));
    let ui = ApplicationUi::new(gallery_tree()?)
        .with_visual_style(visuals)
        .with_text(NativeUiTextConfig::new(FontSource::file(DEV_FONT_FILE)))?
        .with_winit_input(input, |response| {
            for action in response.actions() {
                println!("UI: {action:?}");
            }
        });

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — галерея native UI".to_owned(),
            width: 1280,
            height: 760,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.025, 0.035, 0.07, 1.0))
        // Галерея не анимируется: UI сам запрашивает перерисовку на движение
        // мыши, клик, Tab и активацию. Не тратим кадры впустую в простое.
        .render_loop(RenderLoop::OnDemand)
        .ui(ui)
        .run()?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "The gallery keeps all visibly related widgets together for easy inspection"
)]
fn gallery_tree() -> Result<yuyib::ui::UiTree, yuyib::ui::UiError> {
    let root_panel = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(12, 18, 34)))
        .with_padding(Insets::all(20))
        .with_gap(14);
    let panel = WidgetStyle::default()
        .with_background(ColorToken::Surface)
        .with_padding(Insets::all(16))
        .with_gap(10);
    let muted_panel = WidgetStyle::default()
        .with_background(ColorToken::SurfaceMuted)
        .with_padding(Insets::all(12))
        .with_gap(8);
    let title = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(27, 44, 79)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(12));
    let note = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(24, 39, 65)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(8));
    let warm = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(176, 96, 40)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(8));
    let green = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(39, 132, 93)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(8));

    let buttons = Widget::scroll_view(WidgetId::from_key("gallery-buttons"))
        .with_constraints(fill_both())
        .with_children(vec![
            Widget::container(
                WidgetId::from_key("gallery-buttons-content"),
                LayoutKind::Column,
            )
            .with_constraints(
                LayoutConstraints::auto()
                    .with_width(Dimension::Fill)
                    .with_height(Dimension::Points(900)),
            )
            .with_style(panel)
            .with_children(diagnostic_list(note, green)),
        ]);

    let layout_demo = Widget::container(WidgetId::from_key("gallery-layouts"), LayoutKind::Column)
        .with_constraints(fill_both())
        .with_style(panel)
        .with_children(vec![
            Widget::label(WidgetId::from_key("layouts-title"), "Раскладка и размеры")
                .with_style(caption_style(note)),
            Widget::label(
                WidgetId::from_key("layouts-auto"),
                "Эта подпись имеет Auto: ширина вычисляется по настоящему тексту.",
            )
            .with_style(caption_style(muted_panel)),
            Widget::container(WidgetId::from_key("layouts-row"), LayoutKind::Row)
                .with_constraints(
                    LayoutConstraints::auto()
                        .with_width(Dimension::Fill)
                        .with_height(Dimension::Points(64)),
                )
                .with_style(WidgetStyle::default().with_gap(8))
                .with_children(vec![
                    Widget::label(WidgetId::from_key("layouts-fixed"), "120 px")
                        .with_constraints(
                            LayoutConstraints::auto()
                                .with_width(Dimension::Points(120))
                                .with_height(Dimension::Fill),
                        )
                        .with_style(caption_style(warm)),
                    Widget::label(
                        WidgetId::from_key("layouts-fill"),
                        "Оставшееся место (Fill)",
                    )
                    .with_constraints(fill_both())
                    .with_style(caption_style(green)),
                ]),
            Widget::label(
                WidgetId::from_key("icons-title"),
                "Image / UiImageId (HL). Фон — stand-in; GPU sampling — через extract_image_draw_list.",
            )
            .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
            Widget::container(WidgetId::from_key("icons-row"), LayoutKind::Row)
                .with_constraints(
                    LayoutConstraints::auto()
                        .with_width(Dimension::Fill)
                        .with_height(Dimension::Points(40)),
                )
                .with_style(WidgetStyle::default().with_gap(10))
                .with_children(vec![
                    placeholder_icon("icon-play", UiImageId::new(1), Color::rgb(56, 120, 220)),
                    placeholder_icon("icon-save", UiImageId::new(2), Color::rgb(39, 132, 93)),
                    placeholder_icon("icon-warn", UiImageId::new(3), Color::rgb(176, 96, 40)),
                ]),
            Widget::label(
                WidgetId::from_key("layouts-caption"),
                "Колонка выше + строка: Points, Fill, Auto, padding и gap.",
            )
            .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
        ]);

    // Validates the fullscreen overlay helper compiles/builds; gallery shows an
    // inline twin so Absolute demos remain readable in a column.
    let overlay_content = DialogueOverlayContent::line("Halt. State your business.")
        .with_speaker("Gate Guard")
        .with_choices(vec![
            (
                "dlg-choice:bribe".to_owned(),
                "Maybe this coin will help?".to_owned(),
            ),
            (
                "dlg-choice:leave".to_owned(),
                "I'll come back later.".to_owned(),
            ),
        ]);
    let _overlay = dialogue_overlay_tree(&overlay_content)?;

    let dialogue_demo = Widget::container(
        WidgetId::from_key("gallery-dialogue"),
        LayoutKind::Column,
    )
    .with_constraints(fill_both())
    .with_style(panel)
    .with_children(vec![
        Widget::label(WidgetId::from_key("dialogue-title"), "Диалог / выборы (HL)")
            .with_style(caption_style(note)),
        Widget::label(
            WidgetId::from_key("dialogue-help"),
            "Domain: DialogueSession + StoryFlags. UI: dialogue_overlay_tree + replace_tree.",
        )
        .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
        Widget::label(WidgetId::from_key("dlg-speaker"), "Gate Guard").with_style(
            WidgetStyle::default()
                .with_foreground(ColorToken::Custom(Color::rgb(250, 204, 21)))
                .with_padding(Insets::all(4)),
        ),
        Widget::label(WidgetId::from_key("dlg-body"), "Halt. State your business.").with_style(
            WidgetStyle::default()
                .with_foreground(ColorToken::Text)
                .with_padding(Insets::all(4)),
        ),
        Widget::button(
            WidgetId::from_key("dlg-choice:bribe"),
            "Maybe this coin will help?",
        )
        .with_constraints(fill_width()),
        Widget::button(
            WidgetId::from_key("dlg-choice:leave"),
            "I'll come back later.",
        )
        .with_constraints(fill_width()),
        Widget::label(
            WidgetId::from_key("dialogue-caption"),
            "Клик по выбору → терминал (UiAction). Session/флаги — example dialogue_choice_flow.",
        )
        .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
    ]);

    let controls_demo =
        Widget::container(WidgetId::from_key("gallery-controls"), LayoutKind::Column)
            .with_constraints(fill_both())
            .with_style(panel)
            .with_children(vec![
                Widget::label(
                    WidgetId::from_key("controls-title"),
                    "Checkbox / Toggle / Separator / Spacer",
                )
                .with_style(caption_style(note)),
                Widget::label(
                    WidgetId::from_key("controls-help"),
                    "Новые 2D-виджеты: checkbox/toggle эмиттят UiAction::Toggled.",
                )
                .with_style(caption_style(WidgetStyle::default())),
                Widget::checkbox(WidgetId::from_key("ui-check-sfx"), "Включить звуки", true)
                    .with_style(
                        WidgetStyle::default()
                            .with_foreground(ColorToken::Text)
                            .with_background(ColorToken::Surface)
                            .with_padding(Insets::all(8)),
                    )
                    .with_constraints(fill_width()),
                Widget::toggle(WidgetId::from_key("ui-toggle-vsync"), "VSync", false)
                    .with_style(
                        WidgetStyle::default()
                            .with_foreground(ColorToken::Text)
                            .with_background(ColorToken::Surface)
                            .with_padding(Insets::all(8)),
                    )
                    .with_constraints(fill_width()),
                Widget::separator(WidgetId::from_key("ui-sep")),
                Widget::spacer(WidgetId::from_key("ui-spacer")).with_style(
                    WidgetStyle::default()
                        .with_background(ColorToken::Surface)
                        .with_min_size(Size::new(0, 12)),
                ),
                Widget::label(
                    WidgetId::from_key("controls-caption"),
                    "Separator и Spacer — полезны в статической компоновке.",
                )
                .with_style(caption_style(WidgetStyle::default())),
            ]);

    UiBuilder::new(WidgetId::from_key("gallery-root"), LayoutKind::Column)
        .child(
            Widget::container(WidgetId::from_key("gallery-background"), LayoutKind::Column)
                .with_constraints(fill_both())
                .with_style(root_panel)
                .with_children(vec![
                    Widget::label(
                        WidgetId::from_key("gallery-title"),
                        "Yuyib native UI — текущая галерея возможностей",
                    )
                    .with_constraints(LayoutConstraints::auto().with_width(Dimension::Fill))
                    .with_style(caption_style(title)),
                    Widget::label(
                        WidgetId::from_key("gallery-status"),
                        "ApplicationUi + visual_style + text + Winit; ScrollView drag; Image; Dialogue overlay helper",
                    )
                    .with_constraints(LayoutConstraints::auto().with_width(Dimension::Fill))
                    .with_style(caption_style(note)),
                    Widget::container(WidgetId::from_key("gallery-content"), LayoutKind::Row)
                        .with_constraints(fill_both())
                        .with_style(WidgetStyle::default().with_gap(14))
                        .with_children(vec![buttons, layout_demo, dialogue_demo, controls_demo]),
                    Widget::label(
                        WidgetId::from_key("gallery-footer"),
                        "Сейчас: Container/Label/Button/Image/ScrollView + Checkbox/Toggle/Separator/Spacer; dialogue_overlay_tree; Session в gameplay (example dialogue_choice_flow).",
                    )
                    .with_style(caption_style(note)),
                ]),
        )
        .build()
}

const fn fill_both() -> LayoutConstraints {
    LayoutConstraints::auto()
        .with_width(Dimension::Fill)
        .with_height(Dimension::Fill)
}

const fn fill_width() -> LayoutConstraints {
    LayoutConstraints::auto().with_width(Dimension::Fill)
}

fn placeholder_icon(key: &str, image: UiImageId, color: Color) -> Widget {
    Widget::image(WidgetId::from_key(key), image).with_style(
        WidgetStyle::default()
            .with_background(ColorToken::Custom(color))
            .with_min_size(Size::new(32, 32)),
    )
}

/// `with_style` replaces the whole style (including label min_size). Keep a
/// readable text floor so Auto-height captions do not collapse to content-only
/// when padding is applied later by the host theme.
fn caption_style(style: WidgetStyle) -> WidgetStyle {
    let min_height = style.min_size.height.max(22);
    style
        .with_foreground(ColorToken::Text)
        .with_min_size(Size::new(style.min_size.width, min_height))
}

fn diagnostic_list(note: WidgetStyle, green: WidgetStyle) -> Vec<Widget> {
    let mut rows = vec![
        Widget::label(WidgetId::from_key("buttons-title"), "Кнопки и ScrollView")
            .with_style(caption_style(note)),
        Widget::label(
            WidgetId::from_key("buttons-help"),
            "Мышь / Tab→Enter. Колесо или drag thumb справа; клик по track прыгает к позиции.",
        )
        .with_style(caption_style(WidgetStyle::default())),
        Widget::button(WidgetId::from_key("button-play"), "Запустить")
            .with_constraints(fill_width()),
        Widget::button(WidgetId::from_key("button-save"), "Сохранить")
            .with_constraints(fill_width())
            .with_style(
                green
                    .with_foreground(ColorToken::Text)
                    .with_min_size(Size::new(80, 32)),
            ),
    ];
    for index in 0..20 {
        rows.push(
            Widget::label(
                WidgetId::from_key(&format!("diagnostic-row-{index}")),
                format!("Диагностика #{index:02}: bounded retained UI scroll row"),
            )
            .with_constraints(LayoutConstraints::auto().with_width(Dimension::Fill))
            .with_style(
                WidgetStyle::default()
                    .with_background(ColorToken::SurfaceMuted)
                    .with_foreground(ColorToken::Text)
                    .with_padding(Insets::all(6))
                    .with_min_size(Size::new(0, 28)),
            ),
        );
    }
    rows
}
