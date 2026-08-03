//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "a7b1c203-3333-4333-8333-000000000203"

yuyib_entity! {
    name: "NoDrawSolid",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [-1.3166751861572266, 1.0, 19.5],
        },
        "yuyib.model3d" @ 1: {
            mesh: null,
            model: "builtin:cube",
            render_order: 0,
            visible: true,
        },
        "yuyib.render3d" @ 1: raw {
            {
              "draw": false
            }
        },
        "yuyib.collision3d" @ 1: raw {
            {
              "collide_with": "player",
              "enabled": true,
              "layer": "secret_wall"
            }
        },
    }
}
