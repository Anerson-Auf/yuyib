//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "b5cfeeb6-c1bd-45cd-b929-13a9a40c8c82"

yuyib_entity! {
    name: "Player",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [5.301210403442383, 1.588155746459961, 20.29362678527832],
        },
        "yuyib.model3d" @ 1: {
            mesh: null,
            model: "builtin:cube",
            render_order: 0,
            visible: true,
        },
    }
}
