use super::*;

fn sum(data: &[u8]) -> u64 {
    data.chunks_exact(4).map(|px| u64::from(px[0])).sum()
}

#[test]
fn a_dot_keeps_its_light_wherever_it_lands() {
    for (x, y) in [(5.5, 5.5), (5.0, 5.0), (5.25, 5.8), (5.9, 5.1)] {
        let mut data = vec![0u8; 12 * 12 * 4];
        Frame::new(&mut data, 12, 12, Blend::Add).dot(x, y, [1.0, 1.0, 1.0], 0.8);
        let total = sum(&data);
        assert!((200..=208).contains(&total), "{x},{y}: {total}");
    }
}

#[test]
fn a_line_is_about_as_bright_at_any_angle() {
    let mut totals = Vec::new();
    for (x1, y1) in [(30.0, 10.0), (10.0, 30.0), (24.1, 24.1), (28.0, 18.0)] {
        let mut data = vec![0u8; 40 * 40 * 4];
        Frame::new(&mut data, 40, 40, Blend::Add).line(
            10.0,
            10.0,
            x1,
            y1,
            1.0,
            [1.0, 1.0, 1.0],
            0.5,
            0.5,
        );
        let len = (x1 - 10.0_f32).hypot(y1 - 10.0);
        totals.push(sum(&data) as f32 / len);
    }
    for per_pixel in &totals {
        // A diagonal through the middles of the pixels is the worst case of the coverage sums.
        assert!(
            (per_pixel - totals[0]).abs() < totals[0] * 0.15,
            "{totals:?}"
        );
    }
}

#[test]
fn shapes_off_the_frame_are_skipped_and_edges_are_safe() {
    let mut data = vec![0u8; 8 * 8 * 4];
    let mut frame = Frame::new(&mut data, 8, 8, Blend::Add);
    frame.glow(-40.0, 3.0, 2.0, [1.0; 3], 1.0);
    frame.line(-5.0, -5.0, 20.0, 30.0, 1.5, [1.0; 3], 1.0, 0.0);
    frame.line(f32::NAN, 0.0, 3.0, 3.0, 1.0, [1.0; 3], 1.0, 1.0);
    frame.disc(7.9, 7.9, 3.0, [1.0; 3], 1.0);
    frame.ring(0.0, 0.0, 4.0, 1.0, [1.0; 3], 1.0);
    frame.ellipse(4.0, 4.0, 30.0, 12.0, 1.0, [1.0; 3], 1.0);
    frame.dot(7.9, 7.9, [1.0; 3], 1.0);
    assert!(sum(&data) > 0);
}

#[test]
fn painting_over_keeps_alpha_premultiplied() {
    let mut data = vec![0u8; 6 * 6 * 4];
    let mut frame = Frame::new(&mut data, 6, 6, Blend::Over);
    frame.disc(3.0, 3.0, 2.0, [1.0, 0.5, 0.0], 0.5);
    let px = &data[(3 * 6 + 3) * 4..(3 * 6 + 3) * 4 + 4];
    assert_eq!(px[3], 128);
    assert_eq!(px[0], 128);
    assert_eq!(px[1], 64);
    assert_eq!(px[2], 0);
}

#[test]
fn a_short_buffer_draws_nothing() {
    let mut data = vec![0u8; 10];
    let mut frame = Frame::new(&mut data, 8, 8, Blend::Add);
    frame.disc(2.0, 2.0, 2.0, [1.0; 3], 1.0);
    assert_eq!(frame.height(), 0);
}
