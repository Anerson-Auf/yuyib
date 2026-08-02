//! Типизированное хранилище ресурсов в памяти с устойчивыми ссылками.
//!
//! Основа хранит ресурсы и отличает старую ссылку от новой после удаления.
//! [`AssetLoader`] даёт простой путь фоновой подготовки с публикацией в
//! [`Assets`], а [`AssetLoadQueue`] оставляет полный контроль над моментом
//! публикации. GPU-загрузка остаётся задачей отрисовщика: рабочий поток не
//! может изменить окно, GPU-устройство или ECS-мир.

#![forbid(unsafe_code)]

mod cook;
mod cook_cache;
mod importer;
mod upload;

pub use cook::{
    AssetCooker, CookContext, CookError, CookKey, CookManifest, CookedArtifact, CookerIdentity,
    content_hash_blake3, options_hash_blake3,
};
pub use cook_cache::{CookCache, CookCacheError};

pub use importer::{
    AssetImporter, ImportCancellation, ImportContext, ImportDependency, ImportDependencyKind,
    ImportDiagnostic, ImportDiagnosticSeverity, ImportError, ImportMatch, ImportProbe,
    ImportResult, ImportSource, ImporterDescriptor, ImporterIdentity, ImporterOutput,
    ImporterRegistrationError, ImporterRegistry, ImporterRegistryConfigError,
    ImporterRegistryLimits, OwnedImportSource,
};

pub use upload::{
    AssetUploadBudget, AssetUploadBudgetError, AssetUploadId, AssetUploadPriority,
    AssetUploadQueue, AssetUploadQueueConfig, AssetUploadQueueConfigError, AssetUploadResult,
    AssetUploadSubmitError, AssetUploadUpdate,
};

use std::{
    error::Error,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    num::NonZeroU32,
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use yuyib_tasks::{Task, TaskError, TaskPool, TaskPoolConfig, TaskPoolCreateError, TaskSpawnError};

const PHASE_QUEUED: u8 = 0;
const PHASE_READING: u8 = 1;
const PHASE_DECODING: u8 = 2;

/// A typed asset reference that becomes invalid after its asset is removed.
#[derive(Debug)]
pub struct AssetId<T> {
    index: u32,
    generation: NonZeroU32,
    marker: PhantomData<fn() -> T>,
}

impl<T> Copy for AssetId<T> {}

impl<T> Clone for AssetId<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> PartialEq for AssetId<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<T> Eq for AssetId<T> {}

impl<T> Hash for AssetId<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<T> AssetId<T> {
    fn new(index: u32, generation: NonZeroU32) -> Self {
        Self {
            index,
            generation,
            marker: PhantomData,
        }
    }
}

/// Runtime residency state of a stable asset handle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetState {
    /// The handle is valid, but its value is still being prepared or uploaded.
    Loading,
    /// The value is resident and can be obtained from [`Assets::get`].
    Ready,
    /// Preparation failed; the handle remains valid for diagnostics or retry.
    Failed,
}

/// Reproducibility and memory information retained with one asset slot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AssetMetadata {
    /// Logical source URI or project-relative source path.
    pub source: Option<String>,
    /// Stable importer name and semantic version.
    pub importer_version: Option<String>,
    /// Stable cooker name and semantic version.
    pub cooker_version: Option<String>,
    /// Hex or backend-defined content hash used for cache invalidation.
    pub content_hash: Option<String>,
    /// Logical dependency URIs in deterministic order.
    pub dependencies: Vec<String>,
    /// Estimated resident CPU bytes, if known.
    pub cpu_bytes: Option<u64>,
    /// Estimated resident GPU bytes, if known.
    pub gpu_bytes: Option<u64>,
    /// Bounded importer warnings retained for diagnostics UI.
    pub diagnostics: Vec<String>,
}

/// A worker-prepared value and the final metadata published atomically with it.
#[derive(Debug)]
pub struct PreparedAsset<T> {
    /// Prepared CPU value.
    pub value: T,
    /// Final source, importer, dependency and cost metadata.
    pub metadata: AssetMetadata,
}

impl<T> PreparedAsset<T> {
    /// Creates a prepared value ready for main-thread publication.
    #[must_use]
    pub const fn new(value: T, metadata: AssetMetadata) -> Self {
        Self { value, metadata }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotState {
    Vacant,
    Loading,
    Ready,
    Failed,
}

#[derive(Debug)]
struct Slot<T> {
    generation: NonZeroU32,
    state: SlotState,
    value: Option<T>,
    metadata: AssetMetadata,
}

/// Publishing a prepared value into a stable asset slot failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetPublishError {
    /// The handle is stale, unknown, or has been discarded.
    UnknownHandle,
    /// The slot already contains a ready value.
    AlreadyReady,
}

impl fmt::Display for AssetPublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownHandle => formatter.write_str("asset handle is unknown or stale"),
            Self::AlreadyReady => formatter.write_str("asset handle is already ready"),
        }
    }
}

impl Error for AssetPublishError {}

/// Owns assets of one type and validates their typed handles.
#[derive(Debug, Default)]
pub struct Assets<T> {
    slots: Vec<Slot<T>>,
    vacant: Vec<u32>,
}

impl<T> Assets<T> {
    /// Creates an empty asset store.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            vacant: Vec::new(),
        }
    }

    /// Inserts an asset and returns its stable typed handle.
    ///
    /// # Panics
    ///
    /// Panics only if the process attempts to allocate more than `u32::MAX`
    /// asset slots of one type, which exceeds the representable handle space.
    pub fn insert(&mut self, value: T) -> AssetId<T> {
        self.insert_with_metadata(value, AssetMetadata::default())
    }

    /// Inserts a ready asset together with reproducibility metadata.
    ///
    /// # Panics
    ///
    /// Panics only after exhausting the `u32` handle index space.
    pub fn insert_with_metadata(&mut self, value: T, metadata: AssetMetadata) -> AssetId<T> {
        if let Some(index) = self.vacant.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.value.is_none());
            debug_assert_eq!(slot.state, SlotState::Vacant);
            slot.state = SlotState::Ready;
            slot.value = Some(value);
            slot.metadata = metadata;
            return AssetId::new(index, slot.generation);
        }

        let index = u32::try_from(self.slots.len()).expect("Yuyib asset slot limit exceeded");
        let generation = NonZeroU32::MIN;
        self.slots.push(Slot {
            generation,
            state: SlotState::Ready,
            value: Some(value),
            metadata,
        });
        AssetId::new(index, generation)
    }

    /// Reserves a stable handle before background preparation begins.
    ///
    /// [`Self::get`] returns `None` until [`Self::publish`] succeeds. Rendering
    /// code can keep this handle and choose a typed placeholder from
    /// [`Self::get_or_placeholder`] without replacing ECS components later.
    ///
    /// # Panics
    ///
    /// Panics only after exhausting the `u32` handle index space.
    pub fn reserve(&mut self, metadata: AssetMetadata) -> AssetId<T> {
        if let Some(index) = self.vacant.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert_eq!(slot.state, SlotState::Vacant);
            slot.state = SlotState::Loading;
            slot.metadata = metadata;
            return AssetId::new(index, slot.generation);
        }

        let index = u32::try_from(self.slots.len()).expect("Yuyib asset slot limit exceeded");
        let generation = NonZeroU32::MIN;
        self.slots.push(Slot {
            generation,
            state: SlotState::Loading,
            value: None,
            metadata,
        });
        AssetId::new(index, generation)
    }

    /// Publishes a prepared value into a reserved or failed slot.
    ///
    /// # Errors
    ///
    /// Returns [`AssetPublishError::UnknownHandle`] for stale/discarded handles
    /// and [`AssetPublishError::AlreadyReady`] when replacement was not explicit.
    pub fn publish(&mut self, id: AssetId<T>, value: T) -> Result<(), AssetPublishError> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation && slot.state != SlotState::Vacant)
            .ok_or(AssetPublishError::UnknownHandle)?;
        if slot.state == SlotState::Ready {
            return Err(AssetPublishError::AlreadyReady);
        }
        slot.value = Some(value);
        slot.state = SlotState::Ready;
        Ok(())
    }

    /// Publishes a prepared value and its final metadata as one slot transition.
    ///
    /// # Errors
    ///
    /// Uses the same stale-handle and already-ready rules as [`Self::publish`].
    pub fn publish_prepared(
        &mut self,
        id: AssetId<T>,
        prepared: PreparedAsset<T>,
    ) -> Result<(), AssetPublishError> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation && slot.state != SlotState::Vacant)
            .ok_or(AssetPublishError::UnknownHandle)?;
        if slot.state == SlotState::Ready {
            return Err(AssetPublishError::AlreadyReady);
        }
        slot.value = Some(prepared.value);
        slot.metadata = prepared.metadata;
        slot.state = SlotState::Ready;
        Ok(())
    }

    /// Marks a reserved asset as failed while retaining its stable handle.
    ///
    /// # Errors
    ///
    /// Returns [`AssetPublishError::UnknownHandle`] for a stale handle and
    /// [`AssetPublishError::AlreadyReady`] for a resident value.
    pub fn mark_failed(&mut self, id: AssetId<T>) -> Result<(), AssetPublishError> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation && slot.state != SlotState::Vacant)
            .ok_or(AssetPublishError::UnknownHandle)?;
        if slot.state == SlotState::Ready {
            return Err(AssetPublishError::AlreadyReady);
        }
        slot.value = None;
        slot.state = SlotState::Failed;
        Ok(())
    }

    /// Returns the observable state of a current non-vacant handle.
    #[must_use]
    pub fn state(&self, id: AssetId<T>) -> Option<AssetState> {
        let slot = self
            .slots
            .get(id.index as usize)
            .filter(|slot| slot.generation == id.generation)?;
        match slot.state {
            SlotState::Vacant => None,
            SlotState::Loading => Some(AssetState::Loading),
            SlotState::Ready => Some(AssetState::Ready),
            SlotState::Failed => Some(AssetState::Failed),
        }
    }

    /// Returns retained import/cost metadata for a current handle.
    #[must_use]
    pub fn metadata(&self, id: AssetId<T>) -> Option<&AssetMetadata> {
        let slot = self
            .slots
            .get(id.index as usize)
            .filter(|slot| slot.generation == id.generation && slot.state != SlotState::Vacant)?;
        Some(&slot.metadata)
    }

    /// Returns mutable metadata for publishing measured CPU/GPU costs.
    pub fn metadata_mut(&mut self, id: AssetId<T>) -> Option<&mut AssetMetadata> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation && slot.state != SlotState::Vacant)?;
        Some(&mut slot.metadata)
    }

    /// Returns an asset if the handle is current and resident in this store.
    #[must_use]
    pub fn get(&self, id: AssetId<T>) -> Option<&T> {
        self.slots.get(id.index as usize).and_then(|slot| {
            (slot.generation == id.generation && slot.state == SlotState::Ready)
                .then_some(slot.value.as_ref())
                .flatten()
        })
    }

    /// Returns a resident value or the caller-selected typed placeholder.
    #[must_use]
    pub fn get_or_placeholder<'asset>(
        &'asset self,
        id: AssetId<T>,
        placeholder: &'asset T,
    ) -> &'asset T {
        self.get(id).unwrap_or(placeholder)
    }

    /// Returns a mutable asset if the handle is current and resident in this store.
    pub fn get_mut(&mut self, id: AssetId<T>) -> Option<&mut T> {
        self.slots.get_mut(id.index as usize).and_then(|slot| {
            (slot.generation == id.generation && slot.state == SlotState::Ready)
                .then_some(slot.value.as_mut())
                .flatten()
        })
    }

    /// Removes an asset and invalidates all copies of its old handle.
    pub fn remove(&mut self, id: AssetId<T>) -> Option<T> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation || slot.state != SlotState::Ready {
            return None;
        }
        let value = slot.value.take()?;
        slot.state = SlotState::Vacant;
        slot.metadata = AssetMetadata::default();
        slot.generation =
            NonZeroU32::new(slot.generation.get().wrapping_add(1)).unwrap_or(NonZeroU32::MIN);
        self.vacant.push(id.index);
        Some(value)
    }

    /// Discards a loading, failed, or ready slot and invalidates its handle.
    pub fn discard(&mut self, id: AssetId<T>) -> bool {
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return false;
        };
        if slot.generation != id.generation || slot.state == SlotState::Vacant {
            return false;
        }
        slot.value = None;
        slot.state = SlotState::Vacant;
        slot.metadata = AssetMetadata::default();
        slot.generation =
            NonZeroU32::new(slot.generation.get().wrapping_add(1)).unwrap_or(NonZeroU32::MIN);
        self.vacant.push(id.index);
        true
    }
}

/// Идентификатор одного запроса внутри [`AssetLoadQueue`].
///
/// В отличие от [`AssetId`], это не ссылка на ресурс, а ссылка на работу,
/// которая может подготовить ресурс или завершиться ошибкой.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct AssetLoadId {
    index: u32,
}

/// Наблюдаемое состояние одной фоновой загрузки.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetLoadState {
    /// Работа принята, но ещё не начата рабочим потоком.
    Queued,
    /// Рабочий поток читает байты или другие входные данные.
    Reading,
    /// Рабочий поток декодирует или подготавливает данные в памяти.
    Decoding,
    /// Данные готовы; основной поток может их опубликовать.
    ReadyToPublish,
    /// Рабочий поток или загрузчик вернул ошибку.
    Failed,
    /// Значение забрано шагом публикации в основном потоке.
    Published,
}

impl AssetLoadState {
    /// Возвращает `true`, когда фоновая работа для запроса закончена.
    #[must_use]
    pub const fn is_finished(self) -> bool {
        matches!(self, Self::ReadyToPublish | Self::Failed | Self::Published)
    }
}

/// Копируемые счётчики работы для загрузочного экрана или диагностики.
///
/// Единицу работы выбирает сам загрузчик: байты файла, число изображений или
/// иной полезный объём. Нулевой итог означает, что точный объём неизвестен;
/// тогда интерфейс показывает число запросов.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetLoadProgress {
    /// Число готовых единиц работы.
    pub completed: u64,
    /// Известное общее число единиц либо ноль, когда оно неизвестно.
    pub total: u64,
}

impl AssetLoadProgress {
    /// Возвращает готовность в диапазоне `0.0..=1.0`, если итог известен.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "UI progress bars and renderer uniforms use f32; the ratio remains bounded"
    )]
    pub fn fraction(self) -> Option<f32> {
        (self.total != 0).then(|| self.completed.min(self.total) as f32 / self.total as f32)
    }
}

/// Снимок одного запроса, пригодный для передачи в код интерфейса.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetLoadInfo {
    /// Идентификатор запроса.
    pub id: AssetLoadId,
    /// Короткое имя для пользователя, переданное при постановке в очередь.
    pub label: String,
    /// Текущее состояние.
    pub state: AssetLoadState,
    /// Точные счётчики работы, если их передал загрузчик.
    pub progress: AssetLoadProgress,
}

/// Общие счётчики запросов для загрузочного экрана.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetLoadSummary {
    /// Число принятых запросов.
    pub total: usize,
    /// Запросы, которые ещё не закончились.
    pub pending: usize,
    /// Успешно подготовленные значения, ожидающие публикации.
    pub ready_to_publish: usize,
    /// Запросы с ошибкой.
    pub failed: usize,
    /// Значения, уже забранные шагом публикации.
    pub published: usize,
    /// Готовые единицы работы от всех запросов.
    pub completed_work: u64,
    /// Известные общие единицы работы от всех запросов.
    pub total_work: u64,
}

impl AssetLoadSummary {
    /// Число запросов, перешедших в конечное состояние.
    #[must_use]
    pub const fn finished(self) -> usize {
        self.ready_to_publish + self.failed + self.published
    }

    /// Возвращает долю работы, если хотя бы один запрос сообщил итог.
    #[must_use]
    pub fn work_fraction(self) -> Option<f32> {
        AssetLoadProgress {
            completed: self.completed_work,
            total: self.total_work,
        }
        .fraction()
    }
}

/// Ошибка, полученная от фонового загрузчика.
#[derive(Debug)]
pub enum AssetLoadFailure<E> {
    /// Ошибка самого загрузчика.
    Loader(E),
    /// Рабочий поток запаниковал или остановился до передачи результата.
    Task(TaskError),
}

impl<E: fmt::Display> fmt::Display for AssetLoadFailure<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Loader(error) => write!(formatter, "asset loader failed: {error}"),
            Self::Task(error) => write!(formatter, "asset worker failed: {error}"),
        }
    }
}

impl<E: Error + 'static> Error for AssetLoadFailure<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Loader(error) => Some(error),
            Self::Task(error) => Some(error),
        }
    }
}

/// Причина, по которой запрос не принят ограниченной очередью задач.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetLoadSubmitError {
    /// В очереди задач нет места. Повторите попытку в следующем кадре.
    Full {
        /// Настроенная вместимость очереди задач.
        capacity: usize,
    },
    /// Очередь задач закрыта.
    Closed,
    /// Эта очередь достигла предела числа запросов.
    TooManyRequests,
}

impl fmt::Display for AssetLoadSubmitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { capacity } => {
                write!(formatter, "asset load queue is full (capacity {capacity})")
            }
            Self::Closed => formatter.write_str("asset task pool is closed"),
            Self::TooManyRequests => formatter.write_str("Yuyib asset-load request limit exceeded"),
        }
    }
}

impl Error for AssetLoadSubmitError {}

impl From<TaskSpawnError> for AssetLoadSubmitError {
    fn from(value: TaskSpawnError) -> Self {
        match value {
            TaskSpawnError::Full { capacity } => Self::Full { capacity },
            TaskSpawnError::Closed => Self::Closed,
        }
    }
}

/// Причина, по которой готовое значение нельзя забрать или опубликовать.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssetLoadTakeError {
    /// Идентификатор не принадлежит этой очереди.
    UnknownRequest,
    /// Рабочий поток ещё не закончил запрос.
    NotReady,
    /// Запрос завершился ошибкой; вызовите [`AssetLoadQueue::failure`].
    Failed,
    /// Значение уже забрано или опубликовано.
    AlreadyPublished,
}

impl fmt::Display for AssetLoadTakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownRequest => formatter.write_str("unknown asset-load request"),
            Self::NotReady => formatter.write_str("asset is not ready for main-thread publishing"),
            Self::Failed => formatter.write_str("asset load failed"),
            Self::AlreadyPublished => formatter.write_str("asset value was already published"),
        }
    }
}

impl Error for AssetLoadTakeError {}

#[derive(Debug)]
struct SharedLoadProgress {
    phase: AtomicU8,
    completed: AtomicU64,
    total: AtomicU64,
}

/// Низкоуровневый передатчик прогресса, который получает фоновая функция.
///
/// Он намеренно содержит только атомарные счётчики. Фоновый код сообщает о
/// прогрессе, но не может записать в [`Assets`], изменить ECS или создать GPU-данные.
#[derive(Clone, Debug)]
pub struct AssetLoadReporter {
    shared: Arc<SharedLoadProgress>,
}

impl AssetLoadReporter {
    /// Помечает запрос как чтение входных данных.
    pub fn reading(&self) {
        self.shared.phase.store(PHASE_READING, Ordering::Release);
    }

    /// Помечает запрос как декодирование или подготовку данных.
    pub fn decoding(&self) {
        self.shared.phase.store(PHASE_DECODING, Ordering::Release);
    }

    /// Указывает общее число единиц работы.
    pub fn set_total_work(&self, total: u64) {
        self.shared.total.store(total, Ordering::Release);
        self.shared.completed.fetch_min(total, Ordering::AcqRel);
    }

    /// Заменяет счётчик готовой работы, не выходя за объявленный итог.
    pub fn set_completed_work(&self, completed: u64) {
        let total = self.shared.total.load(Ordering::Acquire);
        self.shared.completed.store(
            if total == 0 {
                completed
            } else {
                completed.min(total)
            },
            Ordering::Release,
        );
    }

    /// Добавляет готовую работу, не выходя за объявленный итог.
    pub fn advance(&self, work: u64) {
        let total = self.shared.total.load(Ordering::Acquire);
        if total == 0 {
            self.shared.completed.fetch_add(work, Ordering::AcqRel);
        } else {
            let _ =
                self.shared
                    .completed
                    .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                        Some(current.saturating_add(work).min(total))
                    });
        }
    }

    fn progress(&self) -> AssetLoadProgress {
        AssetLoadProgress {
            completed: self.shared.completed.load(Ordering::Acquire),
            total: self.shared.total.load(Ordering::Acquire),
        }
    }

    fn phase(&self) -> AssetLoadState {
        match self.shared.phase.load(Ordering::Acquire) {
            PHASE_READING => AssetLoadState::Reading,
            PHASE_DECODING => AssetLoadState::Decoding,
            _ => AssetLoadState::Queued,
        }
    }
}

enum LoadValue<T, E> {
    Loading,
    Ready(T),
    Failed(AssetLoadFailure<E>),
    Published(Option<AssetId<T>>),
}

struct LoadSlot<T, E> {
    label: String,
    reporter: AssetLoadReporter,
    task: Option<Task<Result<T, E>>>,
    value: LoadValue<T, E>,
}

/// Высокоуровневая очередь параллельной подготовки ресурсов в памяти.
///
/// Поставьте по одной функции на ресурс, вызывайте [`Self::poll`] из кадра, а
/// затем [`Self::publish_ready`] в основном потоке. Окно остаётся отзывчивым,
/// а загрузочный экран получает стабильные счётчики. Для собственного шага
/// публикации (например, загрузки в GPU и создания ECS-сущностей) используйте
/// [`Self::take_ready`].
///
/// Очередь сама не читает файлы и не отменяет начатую работу. `try_queue` не
/// ждёт свободного места: повторите `Full` в следующем кадре, не блокируя окно.
pub struct AssetLoadQueue<T, E> {
    slots: Vec<LoadSlot<T, E>>,
}

/// Результат одного неблокирующего шага [`AssetLoader::update`].
///
/// Ссылки из `published` уже принадлежат переданному [`Assets`]. Их можно
/// немедленно передать игровому миру или системе отрисовки, оставаясь в
/// основном потоке.
#[derive(Debug)]
pub struct AssetLoadUpdate<T> {
    /// Число фоновых запросов, закончившихся с успехом или ошибкой в этом шаге.
    pub completed: usize,
    /// Ресурсы, опубликованные в этом шаге, и их устойчивые ссылки.
    pub published: Vec<(AssetLoadId, AssetId<T>)>,
    /// Состояние всей загрузки после публикации.
    pub summary: AssetLoadSummary,
}

/// Простой владелец параллельной загрузки ресурсов.
///
/// Это высокоуровневый путь для ресурсов, которым достаточно попасть в
/// [`Assets`]. Он сам владеет ограниченным пулом рабочих потоков: поставьте
/// работу через [`Self::try_load`], затем один раз за кадр вызовите
/// [`Self::update`]. Метод не ждёт файлов и публикует готовые значения только
/// там, где вызван `update` — обычно в основном потоке приложения.
///
/// Для загрузки в GPU, создания ECS-сущностей или общего с другими подсистемами
/// пула используйте низкоуровневый [`AssetLoadQueue`].
///
/// При уничтожении `AssetLoader` его пул корректно завершает уже принятые
/// задачи. Поэтому фоновые функции должны завершаться за разумное время.
pub struct AssetLoader<T, E> {
    pool: TaskPool,
    queue: AssetLoadQueue<T, E>,
}

struct AssetServerRequest<T> {
    request: AssetLoadId,
    handle: AssetId<T>,
    terminal: bool,
}

/// High-level background asset service with stable pre-residency handles.
///
/// Unlike [`AssetLoader`], a successful [`Self::try_load`] immediately reserves
/// and returns an [`AssetId`]. The same handle transitions through
/// [`AssetState::Loading`] to `Ready` or `Failed`; ECS components therefore do
/// not need path strings or handle replacement after publication.
pub struct AssetServer<T, E> {
    pool: TaskPool,
    queue: AssetLoadQueue<PreparedAsset<T>, E>,
    requests: Vec<AssetServerRequest<T>>,
}

/// One non-blocking main-thread publication step from [`AssetServer`].
#[derive(Debug)]
pub struct AssetServerUpdate<T> {
    /// Background jobs that became ready or failed during this update.
    pub completed_jobs: usize,
    /// Stable handles that became resident during this update.
    pub ready: Vec<AssetId<T>>,
    /// Stable handles that entered the failed state during this update.
    pub failed: Vec<AssetId<T>>,
    /// Aggregate worker-side progress after publication.
    pub summary: AssetLoadSummary,
}

impl<T, E> AssetServer<T, E> {
    /// Creates a service with an owned bounded CPU task pool.
    ///
    /// # Errors
    ///
    /// Returns an error when worker threads cannot be created.
    pub fn new(config: TaskPoolConfig) -> Result<Self, TaskPoolCreateError> {
        Ok(Self {
            pool: TaskPool::new(config)?,
            queue: AssetLoadQueue::new(),
            requests: Vec::new(),
        })
    }

    /// Starts preparing one asset and returns its stable loading handle.
    ///
    /// The slot is reserved only after the bounded task queue accepts the job.
    /// A `Full` result therefore does not leak a handle or mutate `assets`.
    ///
    /// # Errors
    ///
    /// Returns an error if the task queue is full/closed or its request ID
    /// space is exhausted.
    pub fn try_load<Load>(
        &mut self,
        assets: &mut Assets<T>,
        label: impl Into<String>,
        metadata: AssetMetadata,
        loader: Load,
    ) -> Result<AssetId<T>, AssetLoadSubmitError>
    where
        T: Send + 'static,
        E: Send + 'static,
        Load: FnOnce(AssetLoadReporter) -> Result<T, E> + Send + 'static,
    {
        let worker_metadata = metadata.clone();
        self.try_load_prepared(assets, label, metadata, move |reporter| {
            loader(reporter).map(|value| PreparedAsset::new(value, worker_metadata))
        })
    }

    /// Starts a load whose worker computes both the value and final metadata.
    ///
    /// This is the high-level extension point for importer registries and
    /// cookers. The initial metadata is visible while loading; the prepared
    /// metadata replaces it atomically when the value becomes ready.
    ///
    /// # Errors
    ///
    /// Returns an error if the bounded task queue cannot accept the job.
    pub fn try_load_prepared<Load>(
        &mut self,
        assets: &mut Assets<T>,
        label: impl Into<String>,
        initial_metadata: AssetMetadata,
        loader: Load,
    ) -> Result<AssetId<T>, AssetLoadSubmitError>
    where
        T: Send + 'static,
        E: Send + 'static,
        Load: FnOnce(AssetLoadReporter) -> Result<PreparedAsset<T>, E> + Send + 'static,
    {
        let request = self.queue.try_queue(&self.pool, label, loader)?;
        let handle = assets.reserve(initial_metadata);
        self.requests.push(AssetServerRequest {
            request,
            handle,
            terminal: false,
        });
        Ok(handle)
    }

    /// Publishes completed CPU assets and failure states on the calling thread.
    ///
    /// GPU creation intentionally remains a later bounded upload stage. A
    /// renderer can instead use [`AssetLoadQueue::take_ready`] when publication
    /// requires a device-bound transformation.
    ///
    /// # Errors
    ///
    /// Returns a publishing error only if the caller discarded or populated a
    /// handle behind the server while its request was running.
    pub fn update(
        &mut self,
        assets: &mut Assets<T>,
    ) -> Result<AssetServerUpdate<T>, AssetPublishError> {
        let completed_jobs = self.queue.poll();
        let mut ready = Vec::new();
        let mut failed = Vec::new();
        for record in &mut self.requests {
            if record.terminal {
                continue;
            }
            let Some(info) = self.queue.info(record.request) else {
                continue;
            };
            match info.state {
                AssetLoadState::ReadyToPublish => {
                    let prepared = self
                        .queue
                        .take_ready(record.request)
                        .map_err(|_| AssetPublishError::UnknownHandle)?;
                    assets.publish_prepared(record.handle, prepared)?;
                    record.terminal = true;
                    ready.push(record.handle);
                }
                AssetLoadState::Failed => {
                    assets.mark_failed(record.handle)?;
                    record.terminal = true;
                    failed.push(record.handle);
                }
                AssetLoadState::Queued
                | AssetLoadState::Reading
                | AssetLoadState::Decoding
                | AssetLoadState::Published => {}
            }
        }
        Ok(AssetServerUpdate {
            completed_jobs,
            ready,
            failed,
            summary: self.queue.summary(),
        })
    }

    /// Returns worker progress for a handle owned by this server.
    #[must_use]
    pub fn info(&self, handle: AssetId<T>) -> Option<AssetLoadInfo> {
        let request = self
            .requests
            .iter()
            .find(|record| record.handle == handle)?
            .request;
        self.queue.info(request)
    }

    /// Returns a retained loader/task failure for this stable handle.
    #[must_use]
    pub fn failure(&self, handle: AssetId<T>) -> Option<&AssetLoadFailure<E>> {
        let request = self
            .requests
            .iter()
            .find(|record| record.handle == handle)?
            .request;
        self.queue.failure(request)
    }

    /// Returns aggregate worker progress for loading-screen diagnostics.
    #[must_use]
    pub fn summary(&self) -> AssetLoadSummary {
        self.queue.summary()
    }
}

impl<T, E> AssetLoader<T, E> {
    /// Создаёт самостоятельную загрузку с заданным числом рабочих потоков.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку, если операционная система не дала создать поток.
    pub fn new(config: TaskPoolConfig) -> Result<Self, TaskPoolCreateError> {
        Ok(Self {
            pool: TaskPool::new(config)?,
            queue: AssetLoadQueue::new(),
        })
    }

    /// Ставит подготовку одного ресурса без ожидания свободного рабочего места.
    ///
    /// Функция выполняется в фоновом потоке. Ей нельзя трогать окно, GPU или
    /// ECS-мир; для прогресса используйте полученный [`AssetLoadReporter`].
    /// При переполнении вернётся [`AssetLoadSubmitError::Full`]: сохраните
    /// описание запроса и попробуйте ещё раз в следующем кадре.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку, если ограниченная очередь занята или закрыта.
    pub fn try_load<F>(
        &mut self,
        label: impl Into<String>,
        loader: F,
    ) -> Result<AssetLoadId, AssetLoadSubmitError>
    where
        T: Send + 'static,
        E: Send + 'static,
        F: FnOnce(AssetLoadReporter) -> Result<T, E> + Send + 'static,
    {
        self.queue.try_queue(&self.pool, label, loader)
    }

    /// Забирает завершённые фоновые задачи и вставляет готовые ресурсы в `Assets`.
    ///
    /// Не блокирует кадр. Вставка и выдача `AssetId` происходят синхронно в
    /// текущем потоке, поэтому из `published` безопасно создавать объекты игры
    /// сразу после вызова. Причины ошибок остаются доступны через
    /// [`Self::failure`].
    pub fn update(&mut self, assets: &mut Assets<T>) -> AssetLoadUpdate<T> {
        let completed = self.queue.poll();
        let published = self.queue.publish_ready(assets);
        let summary = self.queue.summary();
        AssetLoadUpdate {
            completed,
            published,
            summary,
        }
    }

    /// Возвращает снимок одного запроса для строки загрузочного экрана.
    #[must_use]
    pub fn info(&self, id: AssetLoadId) -> Option<AssetLoadInfo> {
        self.queue.info(id)
    }

    /// Возвращает общие счётчики загрузки.
    #[must_use]
    pub fn summary(&self) -> AssetLoadSummary {
        self.queue.summary()
    }

    /// Возвращает ошибку неудачной фоновой загрузки, если она уже известна.
    #[must_use]
    pub fn failure(&self, id: AssetLoadId) -> Option<&AssetLoadFailure<E>> {
        self.queue.failure(id)
    }

    /// Возвращает настройки принадлежащего этой загрузке пула рабочих потоков.
    #[must_use]
    pub const fn task_pool_config(&self) -> TaskPoolConfig {
        self.pool.config()
    }
}

impl<T, E> Default for AssetLoadQueue<T, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, E> AssetLoadQueue<T, E> {
    /// Создаёт пустую очередь.
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Ставит одну подготовку данных в очередь, не ожидая заполненную очередь задач.
    ///
    /// Функция выполнится в рабочем потоке `TaskPool`. Ей нельзя обращаться к
    /// окну, GPU-устройству, ECS `World` или небезопасному между потоками
    /// хранилищу. Передатчик позволяет сообщать о чтении, декодировании и работе.
    ///
    /// # Errors
    ///
    /// Возвращает `Full`, когда постановка потребовала бы ожидания. Функция
    /// расходуется даже при ошибке, поэтому храните лёгкое описание запроса вне
    /// очереди и создавайте новую функцию для попытки в следующем кадре.
    pub fn try_queue<F>(
        &mut self,
        pool: &TaskPool,
        label: impl Into<String>,
        loader: F,
    ) -> Result<AssetLoadId, AssetLoadSubmitError>
    where
        T: Send + 'static,
        E: Send + 'static,
        F: FnOnce(AssetLoadReporter) -> Result<T, E> + Send + 'static,
    {
        let index =
            u32::try_from(self.slots.len()).map_err(|_| AssetLoadSubmitError::TooManyRequests)?;
        let reporter = AssetLoadReporter {
            shared: Arc::new(SharedLoadProgress {
                phase: AtomicU8::new(PHASE_QUEUED),
                completed: AtomicU64::new(0),
                total: AtomicU64::new(0),
            }),
        };
        let worker_reporter = reporter.clone();
        let task = pool
            .try_spawn(move || {
                worker_reporter.reading();
                loader(worker_reporter)
            })
            .map_err(AssetLoadSubmitError::from)?;
        self.slots.push(LoadSlot {
            label: label.into(),
            reporter,
            task: Some(task),
            value: LoadValue::Loading,
        });
        Ok(AssetLoadId { index })
    }

    /// Наблюдает завершённые фоновые задачи без ожидания.
    ///
    /// Возвращает число запросов, достигших `ReadyToPublish` или `Failed` в
    /// этом вызове. Вызывайте раз в кадр до чтения состояния или публикации.
    pub fn poll(&mut self) -> usize {
        let mut completed = 0;
        for slot in &mut self.slots {
            let Some(task) = slot.task.as_mut() else {
                continue;
            };
            let Ok(Some(result)) = task.try_take() else {
                continue;
            };
            slot.task = None;
            slot.value = match result {
                Ok(Ok(value)) => LoadValue::Ready(value),
                Ok(Err(error)) => LoadValue::Failed(AssetLoadFailure::Loader(error)),
                Err(error) => LoadValue::Failed(AssetLoadFailure::Task(error)),
            };
            completed += 1;
        }
        completed
    }

    /// Возвращает копируемое состояние одного запроса для интерфейса.
    #[must_use]
    pub fn info(&self, id: AssetLoadId) -> Option<AssetLoadInfo> {
        let slot = self.slots.get(id.index as usize)?;
        Some(AssetLoadInfo {
            id,
            label: slot.label.clone(),
            state: state_of(slot),
            progress: slot.reporter.progress(),
        })
    }

    /// Возвращает общие счётчики для загрузочного экрана.
    #[must_use]
    pub fn summary(&self) -> AssetLoadSummary {
        self.slots
            .iter()
            .fold(AssetLoadSummary::default(), |mut summary, slot| {
                summary.total += 1;
                let state = state_of(slot);
                match state {
                    AssetLoadState::ReadyToPublish => summary.ready_to_publish += 1,
                    AssetLoadState::Failed => summary.failed += 1,
                    AssetLoadState::Published => summary.published += 1,
                    AssetLoadState::Queued | AssetLoadState::Reading | AssetLoadState::Decoding => {
                        summary.pending += 1;
                    }
                }
                let progress = slot.reporter.progress();
                summary.completed_work = summary.completed_work.saturating_add(progress.completed);
                summary.total_work = summary.total_work.saturating_add(progress.total);
                summary
            })
    }

    /// Возвращает ошибку завершённого неудачного запроса.
    #[must_use]
    pub fn failure(&self, id: AssetLoadId) -> Option<&AssetLoadFailure<E>> {
        match &self.slots.get(id.index as usize)?.value {
            LoadValue::Failed(error) => Some(error),
            LoadValue::Loading | LoadValue::Ready(_) | LoadValue::Published(_) => None,
        }
    }

    /// Забирает подготовленное значение для собственного шага в основном потоке.
    ///
    /// Это низкоуровневая точка вмешательства: загрузите данные в GPU, создайте
    /// сущности или иначе подключите значение в основном потоке. Запрос станет
    /// `Published`, и взять его повторно нельзя.
    ///
    /// # Errors
    ///
    /// Возвращает ошибку для неизвестного, незавершённого, неудачного либо уже
    /// забранного запроса. Ошибка загрузчика остаётся в [`Self::failure`].
    pub fn take_ready(&mut self, id: AssetLoadId) -> Result<T, AssetLoadTakeError> {
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .ok_or(AssetLoadTakeError::UnknownRequest)?;
        match std::mem::replace(&mut slot.value, LoadValue::Loading) {
            LoadValue::Ready(value) => {
                slot.value = LoadValue::Published(None);
                Ok(value)
            }
            value @ LoadValue::Loading => {
                slot.value = value;
                Err(AssetLoadTakeError::NotReady)
            }
            value @ LoadValue::Failed(_) => {
                slot.value = value;
                Err(AssetLoadTakeError::Failed)
            }
            value @ LoadValue::Published(_) => {
                slot.value = value;
                Err(AssetLoadTakeError::AlreadyPublished)
            }
        }
    }

    /// Вставляет все готовые значения в [`Assets`] в основном потоке.
    ///
    /// Возвращает пары `(запрос, ссылка)`, чтобы игра добавляла объекты только
    /// после готовности ресурса. Неудачные значения доступны через [`Self::failure`].
    pub fn publish_ready(&mut self, assets: &mut Assets<T>) -> Vec<(AssetLoadId, AssetId<T>)> {
        let mut published = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let value = std::mem::replace(&mut slot.value, LoadValue::Loading);
            match value {
                LoadValue::Ready(value) => {
                    let handle = assets.insert(value);
                    slot.value = LoadValue::Published(Some(handle));
                    let Ok(index) = u32::try_from(index) else {
                        continue;
                    };
                    published.push((AssetLoadId { index }, handle));
                }
                other => slot.value = other,
            }
        }
        published
    }

    /// Возвращает ссылку, назначенную [`Self::publish_ready`], если она есть.
    #[must_use]
    pub fn published_handle(&self, id: AssetLoadId) -> Option<AssetId<T>> {
        match &self.slots.get(id.index as usize)?.value {
            LoadValue::Published(Some(handle)) => Some(*handle),
            LoadValue::Published(None)
            | LoadValue::Loading
            | LoadValue::Ready(_)
            | LoadValue::Failed(_) => None,
        }
    }
}

fn state_of<T, E>(slot: &LoadSlot<T, E>) -> AssetLoadState {
    match &slot.value {
        LoadValue::Loading => slot.reporter.phase(),
        LoadValue::Ready(_) => AssetLoadState::ReadyToPublish,
        LoadValue::Failed(_) => AssetLoadState::Failed,
        LoadValue::Published(_) => AssetLoadState::Published,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::mpsc, thread};

    use super::*;
    use yuyib_tasks::TaskPoolConfig;

    fn pool(workers: usize) -> TaskPool {
        TaskPool::new(TaskPoolConfig::new(workers, 8).expect("valid pool config"))
            .expect("task workers start")
    }

    fn poll_until_finished<T, E>(queue: &mut AssetLoadQueue<T, E>, expected_finished: usize) {
        for _ in 0..10_000 {
            queue.poll();
            if queue.summary().finished() >= expected_finished {
                return;
            }
            thread::yield_now();
        }
        panic!("background task did not complete");
    }

    #[test]
    fn removed_handles_do_not_reference_reused_slots() {
        let mut assets = Assets::new();
        let old = assets.insert("old");
        assert_eq!(assets.remove(old), Some("old"));

        let new = assets.insert("new");
        assert_eq!(assets.get(old), None);
        assert_eq!(assets.get(new), Some(&"new"));
    }

    #[test]
    fn handles_are_hashable_without_requiring_asset_hash_or_equality() {
        struct GpuOnlyAsset;

        let mut assets = Assets::new();
        let handle = assets.insert(GpuOnlyAsset);
        let mut cache = HashMap::new();
        cache.insert(handle, "resident");
        assert_eq!(cache.get(&handle), Some(&"resident"));
    }

    #[test]
    fn reserved_handle_keeps_identity_through_publish_and_uses_placeholder() {
        let mut assets = Assets::new();
        let metadata = AssetMetadata {
            source: Some("assets/player.png".to_owned()),
            cpu_bytes: Some(16),
            ..AssetMetadata::default()
        };
        let handle = assets.reserve(metadata.clone());

        assert_eq!(assets.state(handle), Some(AssetState::Loading));
        assert_eq!(assets.get(handle), None);
        assert_eq!(
            assets.get_or_placeholder(handle, &"placeholder"),
            &"placeholder"
        );
        assert_eq!(assets.metadata(handle), Some(&metadata));

        assets
            .publish(handle, "ready")
            .expect("reserved slot accepts value");
        assert_eq!(assets.state(handle), Some(AssetState::Ready));
        assert_eq!(assets.get(handle), Some(&"ready"));
        assert_eq!(
            assets.publish(handle, "replacement"),
            Err(AssetPublishError::AlreadyReady)
        );
    }

    #[test]
    fn failed_or_discarded_handles_have_explicit_state_and_generation() {
        let mut assets = Assets::<String>::new();
        let failed = assets.reserve(AssetMetadata::default());
        assets.mark_failed(failed).expect("loading slot can fail");
        assert_eq!(assets.state(failed), Some(AssetState::Failed));
        assets
            .publish(failed, "retried".to_owned())
            .expect("failed slot can retry");
        assert_eq!(assets.state(failed), Some(AssetState::Ready));
        assert!(assets.discard(failed));
        assert_eq!(assets.state(failed), None);

        let reused = assets.reserve(AssetMetadata::default());
        assert_ne!(failed, reused);
        assert_eq!(assets.state(reused), Some(AssetState::Loading));
    }

    #[test]
    fn asset_server_publishes_without_changing_the_loading_handle() {
        let mut assets = Assets::new();
        let mut server = AssetServer::<String, &'static str>::new(
            TaskPoolConfig::new(1, 4).expect("valid pool"),
        )
        .expect("worker starts");
        let handle = server
            .try_load(
                &mut assets,
                "player",
                AssetMetadata {
                    source: Some("assets/player.txt".to_owned()),
                    ..AssetMetadata::default()
                },
                |reporter| {
                    reporter.decoding();
                    Ok("resident".to_owned())
                },
            )
            .expect("task accepted");
        assert_eq!(assets.state(handle), Some(AssetState::Loading));

        for _ in 0..10_000 {
            let update = server.update(&mut assets).expect("server owns the handle");
            if !update.ready.is_empty() {
                assert_eq!(update.ready, [handle]);
                assert_eq!(assets.get(handle).map(String::as_str), Some("resident"));
                return;
            }
            thread::yield_now();
        }
        panic!("asset server did not publish completed work");
    }

    #[test]
    fn asset_server_retains_failure_on_the_same_handle() {
        let mut assets = Assets::<String>::new();
        let mut server = AssetServer::<String, &'static str>::new(
            TaskPoolConfig::new(1, 4).expect("valid pool"),
        )
        .expect("worker starts");
        let handle = server
            .try_load(
                &mut assets,
                "broken",
                AssetMetadata::default(),
                |_reporter| Err("decode failed"),
            )
            .expect("task accepted");

        for _ in 0..10_000 {
            let update = server.update(&mut assets).expect("server owns the handle");
            if !update.failed.is_empty() {
                assert_eq!(assets.state(handle), Some(AssetState::Failed));
                assert!(matches!(
                    server.failure(handle),
                    Some(AssetLoadFailure::Loader("decode failed"))
                ));
                return;
            }
            thread::yield_now();
        }
        panic!("asset server did not publish failed work");
    }

    #[test]
    fn queue_reports_progress_then_publishes_on_the_calling_thread() {
        let pool = pool(1);
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut queue = AssetLoadQueue::<String, &'static str>::new();
        let request = queue
            .try_queue(&pool, "Карта", move |reporter| {
                reporter.set_total_work(10);
                reporter.advance(4);
                started_sender.send(()).expect("test receiver lives");
                release_receiver.recv().expect("test sender lives");
                reporter.decoding();
                reporter.advance(6);
                Ok("ready".to_owned())
            })
            .expect("queue accepts task");

        started_receiver.recv().expect("worker started");
        let info = queue.info(request).expect("request info");
        assert_eq!(
            info.progress,
            AssetLoadProgress {
                completed: 4,
                total: 10
            }
        );
        assert_eq!(queue.poll(), 0);
        release_sender.send(()).expect("worker waits");
        poll_until_finished(&mut queue, 1);

        assert_eq!(
            queue.info(request).expect("request info").state,
            AssetLoadState::ReadyToPublish
        );
        assert_eq!(queue.summary().finished(), 1);
        let mut assets = Assets::new();
        let published = queue.publish_ready(&mut assets);
        assert_eq!(published.len(), 1);
        assert_eq!(
            assets.get(published[0].1).map(String::as_str),
            Some("ready")
        );
        assert_eq!(queue.published_handle(request), Some(published[0].1));
    }

    #[test]
    fn two_loads_reach_workers_in_parallel() {
        let pool = pool(2);
        let (started_sender, started_receiver) = mpsc::channel();
        let second_started_sender = started_sender.clone();
        let (release_first_sender, release_first_receiver) = mpsc::channel();
        let (release_second_sender, release_second_receiver) = mpsc::channel();
        let mut queue = AssetLoadQueue::<u32, ()>::new();
        queue
            .try_queue(&pool, "Первая часть", move |_| {
                started_sender.send(()).expect("test receiver lives");
                release_first_receiver.recv().expect("test sender lives");
                Ok(1)
            })
            .expect("first queue");
        queue
            .try_queue(&pool, "Вторая часть", move |_| {
                second_started_sender.send(()).expect("test receiver lives");
                release_second_receiver.recv().expect("test sender lives");
                Ok(2)
            })
            .expect("second queue");

        started_receiver.recv().expect("first worker started");
        started_receiver.recv().expect("second worker started");
        release_first_sender.send(()).expect("first worker waits");
        release_second_sender.send(()).expect("second worker waits");
        poll_until_finished(&mut queue, 2);
        assert_eq!(queue.summary().ready_to_publish, 2);
    }

    #[test]
    fn failed_loader_keeps_its_error_for_the_main_thread() {
        let pool = pool(1);
        let mut queue = AssetLoadQueue::<u32, &'static str>::new();
        let request = queue
            .try_queue(&pool, "Повреждённый файл", |_| {
                Err("bad bytes")
            })
            .expect("queue accepts task");
        poll_until_finished(&mut queue, 1);
        assert!(matches!(
            queue.failure(request),
            Some(AssetLoadFailure::Loader("bad bytes"))
        ));
        assert_eq!(queue.take_ready(request), Err(AssetLoadTakeError::Failed));
    }

    #[test]
    fn asset_loader_publishes_completed_values_during_update() {
        let mut loader =
            AssetLoader::<String, ()>::new(TaskPoolConfig::new(1, 8).expect("valid pool config"))
                .expect("worker starts");
        let request = loader
            .try_load("Текст", |reporter| {
                reporter.set_total_work(2);
                reporter.advance(2);
                Ok("готово".to_owned())
            })
            .expect("request is accepted");
        let mut assets = Assets::new();

        let update = (0..10_000)
            .find_map(|_| {
                let update = loader.update(&mut assets);
                if update.published.is_empty() {
                    thread::yield_now();
                    None
                } else {
                    Some(update)
                }
            })
            .expect("background task completes");

        assert_eq!(update.completed, 1);
        assert_eq!(update.summary.published, 1);
        assert_eq!(update.published.len(), 1);
        assert_eq!(update.published[0].0, request);
        assert_eq!(
            assets.get(update.published[0].1).map(String::as_str),
            Some("готово")
        );
        assert_eq!(
            loader.info(request).expect("request info").state,
            AssetLoadState::Published
        );
    }

    #[test]
    fn asset_loader_keeps_loader_error_visible_after_update() {
        let mut loader = AssetLoader::<(), &'static str>::new(
            TaskPoolConfig::new(1, 8).expect("valid pool config"),
        )
        .expect("worker starts");
        let request = loader
            .try_load("Повреждённый ресурс", |_| Err("bad data"))
            .expect("request is accepted");
        let mut assets = Assets::new();

        let summary = (0..10_000)
            .find_map(|_| {
                let update = loader.update(&mut assets);
                if update.summary.failed == 1 {
                    Some(update.summary)
                } else {
                    thread::yield_now();
                    None
                }
            })
            .expect("background task completes");

        assert_eq!(summary.published, 0);
        assert!(matches!(
            loader.failure(request),
            Some(AssetLoadFailure::Loader("bad data"))
        ));
    }
}
