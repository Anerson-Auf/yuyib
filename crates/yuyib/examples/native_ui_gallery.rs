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
//! заданный шрифт, автоматический размер текста и ввод Winit. Нажатия мышью,
//! `Tab`, `Enter` и `Space` печатаются в терминал. Для низкоуровневой работы
//! остаются `UiTree`, `layout_with_measurer`, `UiRenderer` и text-проходы.
//!
//! Колесо мыши над левой колонкой прокручивает длинный диагностический список.

use std::error::Error;

use yuyib::{
    app::{Application, ApplicationUi, NativeUiTextConfig, RenderLoop},
    input::{UiDpiPolicy, WinitUiAdapter},
    platform::WindowConfig,
    render::ClearColor,
    ui::{
        Color, ColorToken, Dimension, Insets, LayoutConstraints, LayoutKind, Point, UiBuilder,
        Widget, WidgetId, WidgetStyle,
    },
    ui_text::FontSource,
};

const DEV_FONT_FILE: &str = r"C:\Windows\Fonts\segoeui.ttf";

fn main() -> Result<(), Box<dyn Error>> {
    let input = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels)?;
    let ui = ApplicationUi::new(gallery_tree()?)
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
        .with_padding(Insets::all(16));
    let note = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(24, 39, 65)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(10));
    let warm = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(176, 96, 40)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(8));
    let green = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(39, 132, 93)))
        .with_foreground(ColorToken::Text)
        .with_padding(Insets::all(8));
    let absolute = |width, height, left, top| {
        LayoutConstraints::auto()
            .with_width(Dimension::Points(width))
            .with_height(Dimension::Points(height))
            .with_absolute_position(Point::new(left, top))
    };

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
                .with_style(note),
            Widget::label(
                WidgetId::from_key("layouts-auto"),
                "Эта подпись имеет Auto: ширина вычисляется по настоящему тексту.",
            )
            .with_style(muted_panel),
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
                        .with_style(warm),
                    Widget::label(
                        WidgetId::from_key("layouts-fill"),
                        "Оставшееся место (Fill)",
                    )
                    .with_constraints(fill_both())
                    .with_style(green),
                ]),
            Widget::label(
                WidgetId::from_key("layouts-caption"),
                "Колонка выше + строка: Points, Fill, Auto, padding и gap.",
            )
            .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
        ]);

    let absolute_demo =
        Widget::container(WidgetId::from_key("gallery-absolute"), LayoutKind::Absolute)
            .with_constraints(fill_both())
            .with_style(panel)
            .with_children(vec![
                Widget::label(
                    WidgetId::from_key("absolute-title"),
                    "Абсолютные координаты",
                )
                .with_constraints(absolute(290, 38, 0, 0))
                .with_style(note),
                Widget::label(WidgetId::from_key("absolute-blue"), "(16, 64)")
                    .with_constraints(absolute(108, 54, 16, 64))
                    .with_style(
                        WidgetStyle::default()
                            .with_background(ColorToken::Accent)
                            .with_padding(Insets::all(8)),
                    ),
                Widget::label(WidgetId::from_key("absolute-orange"), "(148, 108)")
                    .with_constraints(absolute(126, 54, 148, 108))
                    .with_style(warm),
                Widget::label(WidgetId::from_key("absolute-green"), "(54, 174)")
                    .with_constraints(absolute(120, 54, 54, 174))
                    .with_style(green),
                Widget::label(
                    WidgetId::from_key("absolute-caption"),
                    "Контейнер сам ничего не угадывает: у каждого дочернего элемента есть Point.",
                )
                .with_constraints(absolute(310, 52, 0, 258))
                .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
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
                    .with_constraints(
                        LayoutConstraints::auto()
                            .with_width(Dimension::Fill)
                            .with_height(Dimension::Points(62)),
                    )
                    .with_style(title),
                    Widget::label(
                        WidgetId::from_key("gallery-status"),
                        "Высокоуровневый ApplicationUi + текст из явного шрифта + Winit-ввод",
                    )
                    .with_style(note),
                    Widget::container(WidgetId::from_key("gallery-content"), LayoutKind::Row)
                        .with_constraints(fill_both())
                        .with_style(WidgetStyle::default().with_gap(14))
                        .with_children(vec![buttons, layout_demo, absolute_demo]),
                    Widget::label(
                        WidgetId::from_key("gallery-footer"),
                        "Реализовано сейчас: Container, Label, Button; Row, Column, Absolute; Auto, Points, Fill.",
                    )
                    .with_style(note),
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

fn diagnostic_list(note: WidgetStyle, green: WidgetStyle) -> Vec<Widget> {
    let mut rows = vec![
        Widget::label(WidgetId::from_key("buttons-title"), "Кнопки и ввод").with_style(note),
        Widget::label(
            WidgetId::from_key("buttons-help"),
            "Нажмите мышью или Tab → Enter/Space. Прокрутите колесом этот список.",
        )
        .with_style(WidgetStyle::default().with_foreground(ColorToken::Text)),
        Widget::button(WidgetId::from_key("button-play"), "Запустить")
            .with_constraints(fill_width()),
        Widget::button(WidgetId::from_key("button-save"), "Сохранить")
            .with_constraints(fill_width())
            .with_style(green),
    ];
    for index in 0..20 {
        rows.push(
            Widget::label(
                WidgetId::from_key(&format!("diagnostic-row-{index}")),
                format!("Диагностика #{index:02}: bounded retained UI scroll row"),
            )
            .with_constraints(
                LayoutConstraints::auto()
                    .with_width(Dimension::Fill)
                    .with_height(Dimension::Points(30)),
            )
            .with_style(WidgetStyle::default().with_background(ColorToken::SurfaceMuted)),
        );
    }
    rows
}
