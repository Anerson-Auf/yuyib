//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "f739d1f8-7368-416a-ab10-9ddf039f9804"

yuyib_entity! {
    name: "New Entity",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [17.28954315185547, 0.0, -8.593111038208008],
        },
        "yuyib.model3d" @ 1: {
            mesh: null,
            model: "asset://9b6da13b-8edf-4b2b-b978-ef9c06f0dccd",
            render_order: 0,
            visible: true,
        },
        "yuyib.local-transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [17.28954315185547, 0.0, -8.593111038208008],
        },
    }
}
