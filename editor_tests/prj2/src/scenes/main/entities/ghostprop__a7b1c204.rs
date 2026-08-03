//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "a7b1c204-4444-4444-8444-000000000204"

yuyib_entity! {
    name: "GhostProp",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [2.0, 0.699999988079071, 21.631820678710938],
        },
        "yuyib.model3d" @ 1: {
            mesh: null,
            model: "builtin:cube",
            render_order: 0,
            visible: true,
        },
        "yuyib.collision3d" @ 1: raw {
            {
              "collide_with": "",
              "enabled": false,
              "layer": "ghost"
            }
        },
    }
}
