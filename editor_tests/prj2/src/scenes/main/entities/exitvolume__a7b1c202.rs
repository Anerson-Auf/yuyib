//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "a7b1c202-2222-4222-8222-000000000202"

yuyib_entity! {
    name: "ExitVolume",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [4.5, 1.0, 19.5],
        },
        "yuyib.model3d" @ 1: {
            mesh: null,
            model: "builtin:cube",
            render_order: 0,
            visible: true,
        },
        "yuyib.trigger" @ 1: raw {
            {
              "enabled": true,
              "radius": 1.5,
              "trigger": "level.exit"
            }
        },
    }
}
