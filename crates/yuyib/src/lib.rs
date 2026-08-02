//! Yuyib is a native-first Rust runtime for Windows applications and 2D/3D games.
//!
//! The facade exposes composable application, ECS, asset, native UI, 2D/3D,
//! gameplay and import modules without hiding their lower-level crates.
//! Windows `WebView2` support remains an explicit `webview` feature so native
//! applications and games do not pay for a browser dependency by default.

#![forbid(unsafe_code)]

#[cfg(feature = "two-d")]
pub use yuyib_2d as two_d;
#[cfg(feature = "app")]
pub use yuyib_app as app;
#[cfg(feature = "assets")]
pub use yuyib_assets as assets;
#[cfg(feature = "audio")]
pub use yuyib_audio as audio;
#[cfg(feature = "three-d")]
pub use yuyib_character_3d as character_3d;
#[cfg(feature = "core")]
pub use yuyib_core as core;
#[cfg(feature = "ecs")]
pub use yuyib_ecs as ecs;
#[cfg(feature = "game")]
pub use yuyib_game as game;
#[cfg(feature = "two-d")]
pub use yuyib_game_2d as game_2d;
#[cfg(feature = "three-d")]
pub use yuyib_game_3d as game_3d;
#[cfg(feature = "gameplay")]
pub use yuyib_gameplay as gameplay;
#[cfg(feature = "three-d")]
pub use yuyib_gltf as gltf;
#[cfg(feature = "two-d")]
pub use yuyib_image as image;
#[cfg(feature = "three-d")]
pub use yuyib_input as input;
#[cfg(feature = "three-d")]
pub use yuyib_model as model;
#[cfg(feature = "three-d")]
pub use yuyib_model_assets as model_assets;
#[cfg(feature = "net")]
pub use yuyib_net as net;
#[cfg(feature = "physics")]
pub use yuyib_physics as physics;
#[cfg(feature = "platform")]
pub use yuyib_platform as platform;
#[cfg(feature = "render")]
pub use yuyib_render as render;
#[cfg(feature = "two-d")]
pub use yuyib_render_2d as render_2d;
#[cfg(feature = "three-d")]
pub use yuyib_render_3d as render_3d;
#[cfg(feature = "two-d")]
pub use yuyib_render_texture as render_texture;
#[cfg(feature = "three-d")]
pub use yuyib_scene as scene;
#[cfg(feature = "three-d")]
pub use yuyib_shader as shader;
#[cfg(feature = "source1")]
pub use yuyib_source1 as source1;
#[cfg(feature = "source1")]
pub use yuyib_source1_assets as source1_assets;
#[cfg(feature = "source1")]
pub use yuyib_source1_scene as source1_scene;
#[cfg(feature = "tasks")]
pub use yuyib_tasks as tasks;
#[cfg(feature = "ui")]
pub use yuyib_ui as ui;
#[cfg(feature = "ui")]
pub use yuyib_ui_render as ui_render;
#[cfg(feature = "ui")]
pub use yuyib_ui_text as ui_text;
#[cfg(feature = "ui")]
pub use yuyib_ui_text_render as ui_text_render;
#[cfg(feature = "source1")]
pub use yuyib_vmf as vmf;
#[cfg(feature = "source1")]
pub use yuyib_vmf_model as vmf_model;
#[cfg(feature = "source1")]
pub use yuyib_vmt as vmt;
#[cfg(feature = "source1")]
pub use yuyib_vtf as vtf;
#[cfg(feature = "webview")]
pub use yuyib_webview as webview;

/// Imports that are useful in nearly every Yuyib application.
pub mod prelude {
    #[cfg(feature = "app")]
    pub use crate::app::{Application, FrameContext, RenderLoop, WindowEventContext};
    #[cfg(feature = "ui")]
    pub use crate::app::{
        ApplicationUi, NativeUiTextConfig, NativeUiTextError, NativeUiTextInitError,
    };
    #[cfg(feature = "webview")]
    pub use crate::app::{ApplicationWebView, ApplicationWebViewHandle};
    #[cfg(feature = "assets")]
    pub use crate::assets::{
        AssetId, AssetImporter, AssetLoadFailure, AssetLoadId, AssetLoadInfo, AssetLoadProgress,
        AssetLoadQueue, AssetLoadReporter, AssetLoadState, AssetLoadSubmitError, AssetLoadSummary,
        AssetLoadTakeError, AssetLoadUpdate, AssetLoader, AssetMetadata, AssetPublishError,
        AssetServer, AssetServerUpdate, AssetState, AssetUploadBudget, AssetUploadId,
        AssetUploadPriority, AssetUploadQueue, AssetUploadQueueConfig, AssetUploadResult,
        AssetUploadUpdate, Assets, ImportCancellation, ImportContext, ImportDependency,
        ImportDependencyKind, ImportDiagnostic, ImportDiagnosticSeverity, ImportError, ImportMatch,
        ImportProbe, ImportResult, ImportSource, ImporterDescriptor, ImporterIdentity,
        ImporterOutput, ImporterRegistrationError, ImporterRegistry, ImporterRegistryConfigError,
        ImporterRegistryLimits, OwnedImportSource, PreparedAsset,
    };
    #[cfg(feature = "audio")]
    pub use crate::audio::{AudioClip, AudioEngine, AudioLoadLimits};
    #[cfg(feature = "three-d")]
    pub use crate::character_3d::{
        CharacterCollisionError3d, CharacterCollisionResolution3d, CharacterController3d,
        CharacterControllerConfig3d, CharacterControllerEvent3d, CharacterInput3d,
        CharacterModelPlacement3d, CharacterModelPlacementError3d, CharacterMotor3d,
        CharacterMotorConfig3d, CharacterMotorEvent3d, CharacterSpawnAnchor3d,
        CharacterSpawnOptions3d, CharacterSpawnRejectCounts3d, CharacterSpawnRejectReason3d,
        CharacterSpawnReport3d, CharacterSpawnSelection3d, CharacterSpawnSurfaceSelection3d,
        step_character_motors_3d,
    };
    #[cfg(feature = "core")]
    pub use crate::core::{FrameEvents, FrameInfo, Runtime, RuntimeEvent};
    #[cfg(feature = "ecs")]
    pub use crate::ecs::prelude::*;
    #[cfg(feature = "game")]
    pub use crate::game::{
        FixedTime, FixedUpdateConfig, FixedUpdateConfigError, FixedUpdateStats, Game, GameFrame,
        GamePlugin, GameSchedule, GameTime,
    };
    #[cfg(feature = "two-d")]
    pub use crate::game_2d::{
        AnimatedSprite2d, DrawBudget2d, Game2dScene, Game2dSceneConfig, Game2dSceneError,
        Game2dSceneStats, KinematicSpriteController2d, KinematicSpriteControllerError2d,
        KinematicSpriteMove2d, Sprite2d, SpriteAnimationEvent2d, SpriteExtractionLimits2d,
        SpriteMoveInput2d, SpriteViewport2d, TextureCacheConfig2d, TextureQueueError2d,
        TileCollision2d, TileKinematicAabbContact2d, TileKinematicAabbLimits2d,
        TileKinematicAabbMove2d, TileMap2d, TileViewport2d, build_tile_static_colliders_2d,
        extract_tile_collisions_2d, extract_tiles_2d, extract_tiles_chunked_2d,
        extract_visible_sprites_2d, resolve_kinematic_tilemap_aabb_2d,
        step_kinematic_sprite_controller_2d, step_sprite_animations_2d,
        step_tile_map_animations_2d,
    };
    #[cfg(feature = "three-d")]
    pub use crate::game_3d::{
        DirectionalLight3d, LocalMatrixTransform3d, LocalTransform3d, LodGroup3d, LodLevel3d,
        Model3d, Parent3d, SceneBounds3d, SceneBoundsError3d, SceneBoundsResult3d,
        SceneCollisionBuildLimits3d, SceneCollisionError3d, SceneCollisionLimitResource3d,
        StaticSceneCollider3d, StaticSceneCollisionDraw3d, StaticSceneCollisionPrimitive3d,
        Transform3d, WorldTransform3d, build_static_scene_collider_3d,
        build_static_scene_collider_3d_from_draws_with, extract_models_with_lod_3d,
        scene_bounds_3d,
    };
    #[cfg(feature = "gameplay")]
    pub use crate::gameplay::interaction_2d::{
        InteractionLayer2d, PointerInteraction2dConfig, request_pointer_interaction_2d,
    };
    #[cfg(feature = "gameplay")]
    pub use crate::gameplay::interaction_3d::{UseRaycast3dConfig, request_use_raycast_3d};
    #[cfg(feature = "gameplay")]
    pub use crate::gameplay::{
        ActionId, ActionStates, ActionValue, Interactable, InteractionRequested, ObjectiveId,
        QuestBook, QuestDefinition, QuestEventId, QuestId, QuestObjective, QuestSignal,
        QuestStatus, Trigger, WorldInteractionActivation, WorldInteractionEvent,
        WorldInteractionEvents, WorldInteractionState, WorldInteractionTarget,
    };
    #[cfg(feature = "three-d")]
    pub use crate::gltf::{
        AnimationClipIndex, AnimationPlayState, AnimationPlayer, GltfAssetImporter, ImportLimits,
        ImportOptions, ImportPolicy, ImportReport, ImportedAsset, ImportedScene, SkippedPrimitive,
        cook_key_for_gltf_source, decode_imported_asset, encode_imported_asset,
        gltf_imported_cooker_identity, import_options_fingerprint, import_path,
        import_path_with_options, import_scene_bytes_cached, import_scene_bytes_cached_at,
        import_scene_bytes_embedded, import_scene_path, import_scene_path_with_options,
        sample_animation, GLTF_IMPORTED_COOKER_ID, GLTF_IMPORTED_COOK_SCHEMA,
    };
    #[cfg(feature = "two-d")]
    pub use crate::image::{
        DecodePolicy, DecodedImage, ImageEncodeError, ImageFormat, ImageFormatPolicy,
        Rgba8ReferenceMetrics, encode_png_rgba8, reference_metrics_rgba8, write_png_rgba8,
    };
    #[cfg(feature = "three-d")]
    pub use crate::input::{
        CharacterCameraMode3d, CharacterFollowCamera3d, CharacterFollowCameraError3d,
        CollisionAwareThirdPersonCamera3d, FreeCameraAction3d, FreeCameraBindings3d,
        FreeCameraConfig3d, FreeCameraController3d, FreeCameraError3d, FreeCameraEvent3d,
        KeyboardActionMap, MAX_THIRD_PERSON_CAMERA_COLLISION_ITERATIONS,
        MAX_THIRD_PERSON_CAMERA_PROBE_STEPS, PlayerCharacterBindings3d,
        PlayerCharacterControlConfig3d, PlayerCharacterControlError3d, PlayerCharacterControls3d,
        ThirdPersonCameraConfig3d, ThirdPersonCameraError3d, ThirdPersonCameraUpdate3d,
        ThirdPersonOrbit3d, UiDpiPolicy, WinitKeyboardAdapter, WinitUiAdapter, player_actions,
    };
    #[cfg(feature = "three-d")]
    pub use crate::model::{
        Material, MaterialFactorPatch, Mesh, MeshPrimitive, MeshPrimitiveRef, MissingUvBinding,
        Model, ModelHandle, ModelMaterialPolicy, ModelMaterialPolicyError, ModelMaterialUsage,
        ModelMaterialUsageEntry, ModelTexture, ModelTextureSource, ModelTextureUsage,
        ModelTextureUsageEntry, SpecularGlossinessMaterial,
    };
    #[cfg(feature = "three-d")]
    pub use crate::model_assets::{
        ModelTextureBindings, ModelTextureLoader, ResolvedModelTextureSource,
    };
    #[cfg(feature = "net")]
    pub use crate::net::{
        FrameCodec, FrameLimits, JsonConnection, JsonLimits, ProtocolVersion, TcpServer, connect,
    };
    #[cfg(feature = "physics")]
    pub use crate::physics::{
        Aabb2d, Aabb3d, AabbCollider2d, AabbCollider3d, Circle, CircleCollider, Position2d,
        Position3d, Ray2d, Ray3d, SphereCollider3d, SphereMeshResolution3d, StaticAabb2d,
        StaticAabbBroadphase2d, StaticAabbBroadphaseError2d, StaticAabbBroadphaseLimits2d,
        StaticRaycastAabbHit2d, TriangleMesh3d, TriangleMeshError, TriangleMeshQueryError,
        TriangleMeshRayHit3d, Vec2, Vec3, Velocity2d, Velocity3d, resolve_kinematic_aabb_2d,
    };
    #[cfg(feature = "platform")]
    pub use crate::platform::{
        CursorControl, CursorControlOutcome, CursorGrab, Window, WindowConfig, WindowMode,
    };
    #[cfg(feature = "render")]
    pub use crate::render::{
        CapturedFrameRgba8, ClearColor, ColorPostProcess, ColorPostProcessError, BloomConfig,
        ColorGradeConfig, FxaaConfig, FxaaQuality, GraphPassDescriptor, MAX_OFFSCREEN_DIMENSION,
        OFFSCREEN_COLOR_FORMAT, OffscreenRenderer, OffscreenRendererInitError, RenderGraph,
        RenderGraphBuildError, RenderPassId, RenderPhase, RenderResourceId, RenderStatus,
        RenderViewport, RenderViewportError, Renderer, RendererState,
        TEXTURE_READBACK_BYTES_PER_ROW_ALIGNMENT, TextureReadbackError, TextureReadbackFormat,
        ToneMapping, padded_bytes_per_row, read_texture_rgba8,
    };
    #[cfg(feature = "three-d")]
    pub use crate::render_3d::{
        BaseColorSceneRenderer3d, Camera3d, Game3dLighting, Game3dScene, Game3dSceneConfig,
        Game3dSceneError, Game3dSceneStats, Game3dShading, GltfSceneColliderLayer3d,
        GltfSceneColliderLayerId3d, GltfSceneCollisionConfig3d, GltfSceneCollisionConfigError3d,
        GltfSceneCollisionLimits3d, GltfSceneCollisionMatchMode3d, GltfSceneCollisionNameMatch3d,
        GltfSceneCollisionPredicate3d, GltfSceneCollisionSelector3d, GltfSceneGpuProgress,
        GltfSceneLoad, GltfSceneLoadConfig, GltfSceneLoadError, GltfSceneLoadProgress,
        GltfSceneLoadStage, GltfSceneLoadStartError, GpuPbrMesh, LambertLighting3d, LitMaterial3d,
        LitMeshRenderer3d, LitSceneRenderer3d, LoadedGltfScene, LoadedGltfSceneMaterialPolicyError,
        LoadedGltfSceneRenderError, MAX_SKIN_JOINTS, MeshInstance3d, MeshRenderer3d,
        MeshTransform3d, ModelUploadBudget3d, ModelUploadProgress3d, PbrAlphaCutoff3d,
        PbrAlphaMode3d, PbrLighting3d, PbrMaterial3d, PbrMeshRenderer3d, PbrSceneRenderError,
        PreparedSpecularIbl3d, PreparedEquirectEnvironment3d, PreparedSkybox3d, GgxCookConfig, cook_ggx_specular_ibl, GpuSpecularIbl3d, GpuSkybox3d, SkyboxRenderer3d, DirectionalShadowCaster3d, DirectionalShadowConfig, DirectionalShadowPolicy, FactorShadowCasterDraw, GpuDirectionalShadow, TexturedShadowCasterDraw, shadow_coverage_contains, shadow_texel_world_size, SsaoPolicy, SsaoPolicyError, DiffuseIrradianceSh3d, DepthLoad, SceneRenderer3d,
        SkeletalSceneRenderer3d, SkeletalTextureResources, SkinnedMeshRenderer3d,
        StandardMaterial3d, StandardRenderer3d, TexturedLitMaterial3d, TexturedLitMeshRenderer3d,
        TexturedMaterial3d, TexturedMeshRenderer3d, TexturedSkeletalSceneRenderer3d,
        TexturedSkinnedMaterial3d, TexturedSkinnedMeshRenderer3d, UnboundMaterialPolicy3d,
    };
    #[cfg(feature = "two-d")]
    pub use crate::render_texture::{
        AnisotropyFallback, PreparedTextureUpload, TextureCache, TextureMipmapPolicy,
        TextureSampler, TextureSamplingDiagnostics, TextureSamplingPreset,
    };
    #[cfg(feature = "three-d")]
    pub use crate::scene::{SceneSelection, SpawnedScene, spawn_scene};
    #[cfg(feature = "three-d")]
    pub use crate::shader::{ShaderProgram, ShaderPrototype, ShaderSource};
    #[cfg(feature = "tasks")]
    pub use crate::tasks::{TaskPool, TaskPoolConfig};
    #[cfg(feature = "two-d")]
    pub use crate::two_d::{
        ImportedSpriteAnimation, ImportedSpriteAnimationFrame, ImportedSpriteAtlas,
        ImportedSpriteRegion, RuntimeSpriteAtlas, SpriteAnimation, SpriteAnimationState,
        SpriteAtlasBindError, SpriteAtlasImportError, SpriteAtlasImportLimits,
        SpriteAtlasImportLimitsError, SpriteAtlasImporter, SpriteSheet, Texture, TextureHandle,
        TextureRegion, register_sprite_atlas_importer,
    };
    #[cfg(feature = "ui")]
    pub use crate::ui::{
        KeyboardInput, LayoutWithMeasureError, UiAction, UiBuilder, UiInputState, UiMeasurer,
        UiTokens, layout_with_measurer,
    };
    #[cfg(feature = "ui")]
    pub use crate::ui_text::{FontSource, TextEngine, TextLayoutOptions, TextLimits};
    #[cfg(feature = "ui")]
    pub use crate::ui_text_render::{
        GlyphAtlasConfig, TextColor, TextGlyphDrawOptions, TextGlyphRenderer, TextRasterizer,
        TextViewport,
    };
    #[cfg(feature = "webview")]
    pub use crate::webview::{PageEvent, PageSessionId};
}
