//! Высокоуровневый игровой lifecycle поверх [`yuyib_app::Application`].
//!
//! [`Game`] владеет ECS-миром, startup/fixed/update schedules и вызывает один
//! совместимый игровой callback на каждом кадре. [`GamePlugin`] собирает
//! capabilities без global registry. Окно, GPU-surface и presentation остаются
//! у `Application`, поэтому обычная игра не должна вручную склеивать event loop
//! с ECS. Низкоуровневые crates по-прежнему доступны: [`GameFrame::world`],
//! `Application::on_render` и raw WGPU-проходы не скрываются этим фасадом.

#![forbid(unsafe_code)]

use std::{cell::RefCell, error::Error, fmt, rc::Rc, time::Duration};

#[cfg(feature = "ui")]
use yuyib_app::ApplicationUi;
use yuyib_app::{Application, ApplicationError, FrameContext, RenderLoop, WindowEventContext};
use yuyib_core::FrameInfo;
use yuyib_ecs::bevy_ecs::{
    prelude::Resource,
    schedule::{IntoScheduleConfigs, Schedule},
    system::ScheduleSystem,
    world::World,
};
use yuyib_platform::{
    CursorControl, WindowConfig,
    winit::event::{DeviceEvent, WindowEvent},
};
use yuyib_render::{ClearColor, RenderFrame, RenderGraph};

/// Named high-level schedules owned by [`Game`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum GameSchedule {
    /// Runs once after [`Game::on_start`] and before the native event loop.
    Startup,
    /// Runs zero or more times per presentation frame at a fixed timestep.
    FixedUpdate,
    /// Runs exactly once for each presentation frame.
    Update,
}

/// Fixed-update policy for deterministic gameplay and physics systems.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedUpdateConfig {
    /// Duration represented by one fixed simulation tick.
    pub timestep: Duration,
    /// Maximum fixed ticks executed during one presentation frame.
    pub max_steps_per_frame: u32,
    /// Maximum presentation delta accumulated after a stall or debugger pause.
    pub max_frame_delta: Duration,
}

impl FixedUpdateConfig {
    /// Creates a bounded fixed-update policy.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero timestep, zero step budget, or a frame-delta
    /// limit shorter than one fixed tick.
    pub fn new(
        timestep: Duration,
        max_steps_per_frame: u32,
        max_frame_delta: Duration,
    ) -> Result<Self, FixedUpdateConfigError> {
        if timestep.is_zero() {
            return Err(FixedUpdateConfigError::ZeroTimestep);
        }
        if max_steps_per_frame == 0 {
            return Err(FixedUpdateConfigError::ZeroStepBudget);
        }
        if max_frame_delta < timestep {
            return Err(FixedUpdateConfigError::FrameDeltaBelowTimestep);
        }
        Ok(Self {
            timestep,
            max_steps_per_frame,
            max_frame_delta,
        })
    }
}

impl Default for FixedUpdateConfig {
    fn default() -> Self {
        Self {
            timestep: Duration::from_nanos(16_666_667),
            max_steps_per_frame: 8,
            max_frame_delta: Duration::from_millis(250),
        }
    }
}

/// Invalid fixed-update configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixedUpdateConfigError {
    /// A zero timestep would never consume the accumulator.
    ZeroTimestep,
    /// A zero step budget would prevent fixed systems from running.
    ZeroStepBudget,
    /// The accepted frame delta must contain at least one fixed tick.
    FrameDeltaBelowTimestep,
}

impl fmt::Display for FixedUpdateConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroTimestep => formatter.write_str("fixed timestep must be greater than zero"),
            Self::ZeroStepBudget => {
                formatter.write_str("fixed step budget must be greater than zero")
            }
            Self::FrameDeltaBelowTimestep => {
                formatter.write_str("maximum frame delta must include at least one fixed timestep")
            }
        }
    }
}

impl Error for FixedUpdateConfigError {}

/// Presentation-frame timing available to ECS systems in [`GameSchedule::Update`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Resource)]
pub struct GameTime {
    /// Timing supplied by the shared runtime clock.
    pub frame: FrameInfo,
}

/// Deterministic tick information available in [`GameSchedule::FixedUpdate`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Resource)]
pub struct FixedTime {
    /// Zero-based simulation tick.
    pub tick: u64,
    /// Constant duration represented by this simulation tick.
    pub delta: Duration,
    /// Simulation time elapsed before this tick starts.
    pub elapsed: Duration,
}

/// Observable result of fixed-step scheduling for the latest frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Resource)]
pub struct FixedUpdateStats {
    /// Fixed ticks executed during the latest presentation frame.
    pub steps: u32,
    /// Whether excess accumulated time was dropped to keep the frame bounded.
    pub dropped_time: bool,
    /// Fractional progress from the last fixed tick towards the next one.
    pub interpolation_alpha: f32,
}

/// A composable capability that configures a [`Game`] before it starts.
///
/// Plugins add resources, schedules, callbacks, or renderer integrations. They
/// are ordinary caller-owned values and do not require global registration.
pub trait GamePlugin {
    /// Applies this plugin to the game builder.
    fn build(self, game: &mut Game);
}

impl<Build> GamePlugin for Build
where
    Build: FnOnce(&mut Game),
{
    fn build(self, game: &mut Game) {
        self(game);
    }
}

/// Контекст одного игрового кадра.
///
/// Мир доступен только на потоке окна и только на время callback. Это
/// сохраняет границу: фоновые загрузчики не могут незаметно менять ECS, а
/// создание GPU-ресурсов остаётся в `on_render` либо специализированном
/// renderer-е.
pub struct GameFrame<'frame, 'runtime> {
    world: &'frame mut World,
    application: &'frame mut FrameContext<'runtime>,
}

impl GameFrame<'_, '_> {
    /// Возвращает текущий ECS-мир игры.
    pub fn world(&mut self) -> &mut World {
        self.world
    }

    /// Возвращает временные метрики кадра.
    #[must_use]
    pub const fn frame(&self) -> FrameInfo {
        self.application.frame()
    }

    /// Запрашивает штатное завершение игры после текущего callback.
    pub fn request_exit(&mut self) {
        self.application.request_exit();
    }

    /// Меняет режим курсора из игровой логики.
    pub fn set_cursor_control(&mut self, control: CursorControl) {
        self.application.set_cursor_control(control);
    }
}

type GameFrameCallback = Box<dyn for<'frame, 'runtime> FnMut(&mut GameFrame<'frame, 'runtime>)>;
type GameSetupCallback = Box<dyn FnOnce(&mut World)>;

struct GameRuntimeState {
    world: World,
    fixed_update_schedule: Schedule,
    update_schedule: Schedule,
    fixed_update: FixedUpdateConfig,
    accumulator: Duration,
    fixed_tick: u64,
    fixed_elapsed: Duration,
}

impl GameRuntimeState {
    fn advance(&mut self, frame: FrameInfo) -> FixedUpdateStats {
        let Self {
            world,
            fixed_update_schedule,
            update_schedule,
            fixed_update,
            accumulator,
            fixed_tick,
            fixed_elapsed,
        } = self;

        *accumulator = accumulator.saturating_add(frame.delta.min(fixed_update.max_frame_delta));
        let mut steps = 0;
        while *accumulator >= fixed_update.timestep && steps < fixed_update.max_steps_per_frame {
            world.insert_resource(FixedTime {
                tick: *fixed_tick,
                delta: fixed_update.timestep,
                elapsed: *fixed_elapsed,
            });
            fixed_update_schedule.run(world);
            *accumulator = accumulator.saturating_sub(fixed_update.timestep);
            *fixed_tick = fixed_tick.saturating_add(1);
            *fixed_elapsed = fixed_elapsed.saturating_add(fixed_update.timestep);
            steps += 1;
        }

        let dropped_time = *accumulator >= fixed_update.timestep;
        if dropped_time {
            let remainder = accumulator.as_nanos() % fixed_update.timestep.as_nanos();
            *accumulator = Duration::from_nanos(u64::try_from(remainder).unwrap_or(u64::MAX));
        }
        let interpolation_alpha = accumulator.as_secs_f32() / fixed_update.timestep.as_secs_f32();
        let stats = FixedUpdateStats {
            steps,
            dropped_time,
            interpolation_alpha,
        };
        world.insert_resource(GameTime { frame });
        world.insert_resource(stats);
        update_schedule.run(world);
        stats
    }
}

/// Высокоуровневый builder игры: окно + ECS-мир + один игровой callback.
///
/// `Game` решает только lifecycle. Он не выбирает за игру физику, рендерер,
/// сетевую модель или расписание систем. Их можно добавить в `on_start` и
/// `on_frame`, а отрисовку выполнить через [`Self::on_render`].
pub struct Game {
    application: Application,
    world: World,
    startup_schedule: Schedule,
    fixed_update_schedule: Schedule,
    update_schedule: Schedule,
    fixed_update: FixedUpdateConfig,
    on_start: Option<GameSetupCallback>,
    on_frame: Option<GameFrameCallback>,
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

impl Game {
    /// Создаёт игру с пустым ECS-миром и обычными настройками окна.
    #[must_use]
    pub fn new() -> Self {
        Self {
            application: Application::new().render_loop(RenderLoop::Continuous),
            world: World::new(),
            startup_schedule: Schedule::default(),
            fixed_update_schedule: Schedule::default(),
            update_schedule: Schedule::default(),
            fixed_update: FixedUpdateConfig::default(),
            on_start: None,
            on_frame: None,
        }
    }

    /// Заменяет настройки главного окна.
    #[must_use]
    pub fn window(mut self, config: WindowConfig) -> Self {
        self.application = self.application.window(config);
        self
    }

    /// Задаёт цвет, видимый до пользовательских GPU-проходов.
    #[must_use]
    pub fn clear_color(mut self, color: ClearColor) -> Self {
        self.application = self.application.clear_color(color);
        self
    }

    /// Выбирает перерисовку по событию или непрерывный игровой цикл.
    #[must_use]
    pub fn render_loop(mut self, render_loop: RenderLoop) -> Self {
        self.application = self.application.render_loop(render_loop);
        self
    }

    /// Replaces the bounded fixed-update policy.
    #[must_use]
    pub const fn fixed_update(mut self, config: FixedUpdateConfig) -> Self {
        self.fixed_update = config;
        self
    }

    /// Adds a capability plugin to this game.
    ///
    /// Plugin construction is synchronous and happens before the window or GPU
    /// device exists. A plugin should register startup/update systems here and
    /// defer device resources to render initialization callbacks.
    #[must_use]
    pub fn add_plugin(mut self, plugin: impl GamePlugin) -> Self {
        plugin.build(&mut self);
        self
    }

    /// Returns one mutable schedule for advanced configuration.
    pub fn schedule_mut(&mut self, schedule: GameSchedule) -> &mut Schedule {
        match schedule {
            GameSchedule::Startup => &mut self.startup_schedule,
            GameSchedule::FixedUpdate => &mut self.fixed_update_schedule,
            GameSchedule::Update => &mut self.update_schedule,
        }
    }

    /// Adds systems that run once before the window event loop starts.
    #[must_use]
    pub fn add_startup_systems<Marker>(
        mut self,
        systems: impl IntoScheduleConfigs<ScheduleSystem, Marker>,
    ) -> Self {
        self.startup_schedule.add_systems(systems);
        self
    }

    /// Adds deterministic fixed-step gameplay or physics systems.
    #[must_use]
    pub fn add_fixed_update_systems<Marker>(
        mut self,
        systems: impl IntoScheduleConfigs<ScheduleSystem, Marker>,
    ) -> Self {
        self.fixed_update_schedule.add_systems(systems);
        self
    }

    /// Adds systems that run once per presentation frame.
    #[must_use]
    pub fn add_update_systems<Marker>(
        mut self,
        systems: impl IntoScheduleConfigs<ScheduleSystem, Marker>,
    ) -> Self {
        self.update_schedule.add_systems(systems);
        self
    }

    /// Задаёт начальное состояние игрового курсора.
    #[must_use]
    pub fn cursor_control(mut self, control: CursorControl) -> Self {
        self.application = self.application.cursor_control(control);
        self
    }

    /// Подключает нативное retained UI поверх игровой сцены.
    #[cfg(feature = "ui")]
    #[must_use]
    pub fn ui(mut self, ui: ApplicationUi) -> Self {
        self.application = self.application.ui(ui);
        self
    }

    /// Наблюдает обычные window-события до встроенной обработки.
    ///
    /// Здесь удобно передать клавиатуру, абсолютную мышь или UI-ввод в свой
    /// адаптер. Мутация ECS остаётся в [`Self::on_frame`].
    #[must_use]
    pub fn on_window_event(
        mut self,
        callback: impl FnMut(&WindowEvent, &mut WindowEventContext) + 'static,
    ) -> Self {
        self.application = self.application.on_window_event(callback);
        self
    }

    /// Наблюдает низкоуровневые device-события, включая относительное движение
    /// мыши при захваченном курсоре.
    #[must_use]
    pub fn on_device_event(
        mut self,
        callback: impl FnMut(&DeviceEvent, &mut WindowEventContext) + 'static,
    ) -> Self {
        self.application = self.application.on_device_event(callback);
        self
    }

    /// Передаёт уже созданный ECS-мир.
    #[must_use]
    pub fn world(mut self, world: World) -> Self {
        self.world = world;
        self
    }

    /// Даёт доступ к миру для подготовки до запуска окна.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Вызывает один раз перед созданием окна и началом event loop.
    #[must_use]
    pub fn on_start(mut self, callback: impl FnOnce(&mut World) + 'static) -> Self {
        self.on_start = Some(Box::new(callback));
        self
    }

    /// Вызывает игровую логику на каждом кадре перед пользовательским GPU-проходом.
    #[must_use]
    pub fn on_frame(
        mut self,
        callback: impl for<'frame, 'runtime> FnMut(&mut GameFrame<'frame, 'runtime>) + 'static,
    ) -> Self {
        self.on_frame = Some(Box::new(callback));
        self
    }

    /// Регистрирует низкоуровневую GPU-отрисовку после игровой логики.
    ///
    /// ECS-мир намеренно не передаётся сюда: render extraction должен создавать
    /// snapshot в `on_frame`, чтобы GPU callback не стал второй скрытой фазой
    /// мутации мира.
    #[must_use]
    pub fn on_render(
        mut self,
        callback: impl for<'frame> FnMut(&mut RenderFrame<'frame>) + 'static,
    ) -> Self {
        self.application = self.application.on_render(callback);
        self
    }

    /// Installs a declared render graph before the low-level render callback.
    #[must_use]
    pub fn render_graph(mut self, graph: RenderGraph) -> Self {
        self.application = self.application.render_graph(graph);
        self
    }

    /// Запускает окно и игровой lifecycle.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку окна, GPU-surface или event loop, которую выдал
    /// `Application`.
    pub fn run(mut self) -> Result<(), ApplicationError> {
        if let Some(on_start) = self.on_start.take() {
            on_start(&mut self.world);
        }
        self.startup_schedule.run(&mut self.world);
        let state = Rc::new(RefCell::new(GameRuntimeState {
            world: self.world,
            fixed_update_schedule: self.fixed_update_schedule,
            update_schedule: self.update_schedule,
            fixed_update: self.fixed_update,
            accumulator: Duration::ZERO,
            fixed_tick: 0,
            fixed_elapsed: Duration::ZERO,
        }));
        let callback = Rc::new(RefCell::new(self.on_frame));
        self.application
            .on_frame(move |application| {
                let mut state = state.borrow_mut();
                state.advance(application.frame());
                if let Some(callback) = callback.borrow_mut().as_mut() {
                    callback(&mut GameFrame {
                        world: &mut state.world,
                        application,
                    });
                }
            })
            .run()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        FixedTime, FixedUpdateConfig, FixedUpdateConfigError, Game, GameRuntimeState, GameTime,
    };
    use yuyib_core::FrameInfo;
    use yuyib_ecs::bevy_ecs::prelude::Resource;
    use yuyib_ecs::bevy_ecs::{schedule::Schedule, world::World};

    #[derive(Resource)]
    struct Marker(u32);

    #[derive(Default, Resource)]
    struct ScheduleCounts {
        fixed: u32,
        update: u32,
        latest_tick: Option<u64>,
        latest_frame: Option<u64>,
    }

    #[allow(clippy::needless_pass_by_value)] // Bevy ECS system parameters are value wrappers.
    fn fixed_system(
        time: yuyib_ecs::bevy_ecs::prelude::Res<FixedTime>,
        mut counts: yuyib_ecs::bevy_ecs::prelude::ResMut<ScheduleCounts>,
    ) {
        counts.fixed += 1;
        counts.latest_tick = Some(time.tick);
    }

    #[allow(clippy::needless_pass_by_value)] // Bevy ECS system parameters are value wrappers.
    fn update_system(
        time: yuyib_ecs::bevy_ecs::prelude::Res<GameTime>,
        mut counts: yuyib_ecs::bevy_ecs::prelude::ResMut<ScheduleCounts>,
    ) {
        counts.update += 1;
        counts.latest_frame = Some(time.frame.index);
    }

    #[test]
    fn startup_callback_can_prepare_the_owned_world() {
        let mut game = Game::new().on_start(|world| {
            world.insert_resource(Marker(7));
        });

        let startup = game.on_start.take().expect("callback must be retained");
        startup(game.world_mut());

        assert_eq!(game.world_mut().resource::<Marker>().0, 7);
    }

    #[test]
    fn fixed_update_configuration_rejects_unbounded_or_inert_values() {
        assert_eq!(
            FixedUpdateConfig::new(Duration::ZERO, 1, Duration::from_millis(1)),
            Err(FixedUpdateConfigError::ZeroTimestep)
        );
        assert_eq!(
            FixedUpdateConfig::new(Duration::from_millis(10), 0, Duration::from_millis(10)),
            Err(FixedUpdateConfigError::ZeroStepBudget)
        );
        assert_eq!(
            FixedUpdateConfig::new(Duration::from_millis(10), 1, Duration::from_millis(5)),
            Err(FixedUpdateConfigError::FrameDeltaBelowTimestep)
        );
    }

    #[test]
    fn schedules_run_with_bounded_fixed_catch_up_and_observable_time() {
        let mut fixed = Schedule::default();
        fixed.add_systems(fixed_system);
        let mut update = Schedule::default();
        update.add_systems(update_system);
        let mut world = World::new();
        world.insert_resource(ScheduleCounts::default());
        let mut state = GameRuntimeState {
            world,
            fixed_update_schedule: fixed,
            update_schedule: update,
            fixed_update: FixedUpdateConfig::new(
                Duration::from_millis(10),
                3,
                Duration::from_millis(100),
            )
            .expect("valid fixed policy"),
            accumulator: Duration::ZERO,
            fixed_tick: 0,
            fixed_elapsed: Duration::ZERO,
        };

        let advance_stats = state.advance(FrameInfo {
            index: 9,
            delta: Duration::from_millis(45),
            elapsed: Duration::from_millis(45),
        });

        assert_eq!(advance_stats.steps, 3);
        assert!(advance_stats.dropped_time);
        assert!((advance_stats.interpolation_alpha - 0.5).abs() < f32::EPSILON);
        let counts = state.world.resource::<ScheduleCounts>();
        assert_eq!(counts.fixed, 3);
        assert_eq!(counts.update, 1);
        assert_eq!(counts.latest_tick, Some(2));
        assert_eq!(counts.latest_frame, Some(9));
    }

    #[test]
    fn closure_plugin_can_configure_world_and_schedules() {
        let mut game = Game::new().add_plugin(|game: &mut Game| {
            game.world_mut().insert_resource(Marker(11));
            game.schedule_mut(super::GameSchedule::Startup).add_systems(
                |mut marker: yuyib_ecs::bevy_ecs::prelude::ResMut<Marker>| {
                    marker.0 += 1;
                },
            );
        });

        game.startup_schedule.run(&mut game.world);
        assert_eq!(game.world.resource::<Marker>().0, 12);
    }
}
