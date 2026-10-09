use super::ReflectionParams;

const SURFACES: [[f32; 2]; 6] = [
    [0., 0.],
    [40., 28.],
    [96., 96.],
    [400., 300.],
    [2560., 1440.],
    [5120., 2880.],
];

#[test]
fn disabled_size_scaling_preserves_the_explicit_radius() {
    let params = ReflectionParams {
        diffuse_radius: 123.25,
        diffuse_size_scaling: 0.,
        diffuse_radius_limit: 240.,
        ..Default::default()
    };
    for [width, height] in SURFACES {
        assert_eq!(params.diffuse_radius_for_size(width, height), 123.25);
    }
}

#[test]
fn gradual_surface_growth_has_no_radius_jumps_or_reversals() {
    let params = ReflectionParams {
        diffuse_radius: 34.,
        diffuse_size_scaling: 0.7,
        diffuse_radius_limit: 240.,
        ..Default::default()
    };
    let mut previous = params.diffuse_radius_for_size(0., 0.);
    for side in 1..=2048 {
        let radius = params.diffuse_radius_for_size(side as f32, side as f32);
        assert!(radius >= previous, "radius decreased at side {side}");
        assert!(radius - previous < 0.5, "radius jumped at side {side}");
        assert!((34. ..=240.).contains(&radius));
        previous = radius;
    }
    assert!(params.diffuse_radius_for_size(300., 200.) > 34.);
}

#[test]
fn size_growth_stops_at_the_configured_limit() {
    let params = ReflectionParams {
        diffuse_radius: 34.,
        diffuse_size_scaling: 1.,
        diffuse_radius_limit: 180.,
        ..Default::default()
    };
    assert_eq!(params.diffuse_radius_for_size(512., 512.), 180.);
    assert_eq!(params.diffuse_radius_for_size(5120., 2880.), 180.);
}

#[test]
fn a_manually_larger_radius_is_not_reduced_by_the_growth_limit() {
    let params = ReflectionParams {
        diffuse_radius: 512.,
        diffuse_size_scaling: 1.,
        diffuse_radius_limit: 240.,
        ..Default::default()
    };
    for [width, height] in SURFACES {
        assert_eq!(params.diffuse_radius_for_size(width, height), 512.);
    }
}

#[test]
fn zero_radius_stays_disabled_at_every_surface_size() {
    let params = ReflectionParams {
        diffuse_radius: 0.,
        diffuse_size_scaling: 1.,
        diffuse_radius_limit: 240.,
        ..Default::default()
    };
    for [width, height] in SURFACES {
        assert_eq!(params.diffuse_radius_for_size(width, height), 0.);
    }
}

#[test]
fn size_scaling_does_not_depend_on_sampling_reach() {
    let near = ReflectionParams {
        sampling_radius: 8.,
        ..Default::default()
    };
    let far = ReflectionParams {
        sampling_radius: 240.,
        ..near
    };
    for [width, height] in SURFACES {
        assert_eq!(
            near.diffuse_radius_for_size(width, height),
            far.diffuse_radius_for_size(width, height),
        );
    }
}
