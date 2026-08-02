//! yuyib.entity-projection@1
//! scene_guid = "c934b824-d259-4b1e-a1e4-26a72dde349f"
//! entity_guid = "a7b1c201-1111-4111-8111-000000000201"

yuyib_entity! {
    name: "TalkNpc",
    components: {
        "yuyib.transform3d" @ 1: {
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
            translation: [8.827106475830078, 1.0, 19.5],
        },
        "yuyib.model3d" @ 1: {
            mesh: null,
            model: "builtin:cube",
            render_order: 0,
            visible: true,
        },
        "yuyib.interactable" @ 1: raw {
            {
              "enabled": true,
              "interaction": "world.talk_npc",
              "max_distance": 3.0
            }
        },
    }
}
