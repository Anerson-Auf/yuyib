//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "d489ada5-78be-42f0-bf04-3ff414423889"

yuyib_entity! {
    name: "light",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [-0.015760082751512527, 0.5092620849609375, 0.18403629958629608, 0.8405560255050659],
            scale: [1.0, 1.0, 1.0],
            translation: [0.9494297504425049, 12.92503833770752, -8.297065734863281],
        },
        "yuyib.directional-light3d" @ 1: {
            color: [1.0, 0.95, 0.9],
            direction: [-0.35, -1.0, -0.45],
            enabled: true,
            illuminance_lux: 8.0,
        },
    }
}
