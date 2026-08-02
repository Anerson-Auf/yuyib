//! M4.6 smoke: contact pairs + rope joint + fixed stepper.
//!
//! Headless. Does not touch [`yuyib::physics::TriangleMesh3d`].
//!
//! ```text
//! cargo run -p yuyib --example physics_rapier_contacts_smoke --features physics-rapier
//! ```

use std::error::Error;

use yuyib::physics::{
    DynamicsBackend3d, DynamicsFixedStepper3d, DynamicsWorldConfig3d, RapierDynamicsWorld3d,
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let ground = world.insert_fixed_cuboid([0.0, -0.25, 0.0], [3.0, 0.25, 3.0])?;
    let ball = world.insert_dynamic_sphere([0.0, 2.5, 0.0], 0.4)?;

    let mut stepper = DynamicsFixedStepper3d::hz60();
    for _ in 0..180 {
        let _ = stepper.step_backend(&mut world, stepper.fixed_dt())?;
    }

    let contacts = world.collect_contact_pairs();
    let has_ground_contact = contacts.iter().any(|pair| {
        (pair.body_a == ground && pair.body_b == ball)
            || (pair.body_a == ball && pair.body_b == ground)
    });
    if !has_ground_contact {
        return Err(format!(
            "physics_rapier_contacts_smoke: missing ground contact, pairs={contacts:?}"
        )
        .into());
    }

    let mut rope_world = RapierDynamicsWorld3d::new(DynamicsWorldConfig3d::earth_60hz())?;
    let anchor = rope_world.insert_fixed_cuboid([0.0, 3.0, 0.0], [0.15, 0.15, 0.15])?;
    let bob = rope_world.insert_dynamic_sphere([0.0, 1.0, 0.0], 0.25)?;
    let _rope = rope_world.insert_rope_joint(anchor, bob, 1.2, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0])?;
    let mut rope_stepper = DynamicsFixedStepper3d::hz60();
    for _ in 0..180 {
        let _ = rope_stepper.step_backend(&mut rope_world, rope_stepper.fixed_dt())?;
    }
    let bob_t = rope_world
        .translation(bob)
        .ok_or("physics_rapier_contacts_smoke: missing bob")?;
    let dist = {
        let dx = bob_t[0];
        let dy = bob_t[1] - 3.0;
        let dz = bob_t[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    };
    if dist > 1.35 {
        return Err(format!(
            "physics_rapier_contacts_smoke: rope failed, dist={dist}"
        )
        .into());
    }

    println!(
        "physics_rapier_contacts_smoke OK: contacts={}, rope_dist={dist:.2}",
        contacts.len()
    );
    Ok(())
}
