use super::*;
use crate::raster::Blend;

fn render(normal: Vec3) -> Vec<u8> {
    let mut data = vec![0u8; 320 * 320 * 4];
    for px in data.chunks_exact_mut(4) {
        px.copy_from_slice(&[40, 40, 40, 255]);
    }
    let mut hole = Hole::new();
    let mut frame = Frame::new(&mut data, 320, 320, Blend::Add);
    hole.draw(
        &mut frame,
        &HoleView {
            x: 160.0,
            y: 160.0,
            radius: 22.0,
            normal,
            time: 3.0,
            gain: 1.0,
        },
    );
    data
}

fn at(data: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * 320 + x) * 4;
    [data[i], data[i + 1], data[i + 2]]
}

fn brightest(data: &[u8], x: usize, rows: std::ops::Range<usize>) -> u8 {
    rows.map(|y| at(data, x, y)[0]).max().unwrap_or(0)
}

#[test]
fn the_shadow_is_veiled_and_the_far_side_rings_it() {
    // Seen from a little above the disc.
    let pitch = 0.11_f32;
    let data = render(Vec3::new(0.0, pitch.cos(), -pitch.sin()));
    // Inside the shadow the grey behind is gone; what is left is the warm haze of the light around it.
    let inside = at(&data, 160, 160 - 9);
    assert!(inside[0] < 130 && inside[0] > inside[2], "{inside:?}");
    // The far side, lifted over the shadow and under it: much brighter than the shadow.
    assert!(brightest(&data, 160, 160 - 34..160 - 23) > 170);
    assert!(brightest(&data, 160, 160 + 23..160 + 32) > 150);
    // The bar of the edge-on disc runs out to the sides, burnt out to white near the hole.
    assert!(brightest(&data, 160 - 40, 156..164) > 240);
    assert!(brightest(&data, 160 + 70, 150..170) > 70);
    // Far from the hole the background is left alone.
    assert_eq!(at(&data, 4, 4), [40, 40, 40]);
}

#[test]
fn seen_from_above_the_disc_is_wide_and_brighter_where_the_gas_comes_closer() {
    let pitch = 0.6_f32;
    let data = render(Vec3::new(0.0, pitch.cos(), -pitch.sin()));
    // Gas far out to the sides, and below the hole at about half that.
    assert!(brightest(&data, 160 - 70, 150..170) > 60);
    assert!(brightest(&data, 160 + 70, 150..170) > 40);
    assert!(at(&data, 160, 160 + 40)[0] > 50);
    let left: u32 = (150..170)
        .map(|y| u32::from(at(&data, 160 - 50, y)[0]))
        .sum();
    let right: u32 = (150..170)
        .map(|y| u32::from(at(&data, 160 + 50, y)[0]))
        .sum();
    assert!(left > right + 200, "{left} against {right}");
}

#[test]
fn the_patterns_are_even_and_made_of_strands() {
    let hole = Hole::new();
    let stats = |p: &Pattern| -> (f32, f32) {
        let mean = p.cells.iter().sum::<f32>() / p.cells.len() as f32;
        let spread =
            (p.cells.iter().map(|c| (c - mean).powi(2)).sum::<f32>() / p.cells.len() as f32).sqrt();
        (mean, spread)
    };
    for mips in [&hole.gas.strands, &hole.gas.rings] {
        assert!(mips.levels.len() >= 4);
        for level in &mips.levels {
            let (mean, _) = stats(level);
            assert!((0.6..1.4).contains(&mean), "{mean}");
        }
        // The finest level has dark gaps and bright lines; the coarser ones even them out.
        assert!(stats(&mips.levels[0]).1 > stats(&mips.levels[3]).1 * 1.1);
    }
}

#[test]
fn a_baked_pattern_turns_with_the_gas_and_mixes_two_levels() {
    let hole = Hole::new();
    let mips = &hole.gas.strands;
    let mut still = Pattern {
        across: 2,
        around: 2,
        cells: Vec::new(),
    };
    let mut turned = Pattern {
        across: 2,
        around: 2,
        cells: Vec::new(),
    };
    mips.bake(1.0, |_| (0.0, 0.0), 1.0, &mut still);
    assert_eq!(still.across, mips.levels[1].across);
    assert_eq!(still.cells, mips.levels[1].cells);
    // A quarter of a turn on: the same pattern, a quarter of the way round.
    mips.bake(1.0, |_| (0.25, 0.0), 1.0, &mut turned);
    let cols = still.around;
    assert!((turned.cells[5 * cols + 10] - still.cells[5 * cols + 10 + cols / 4]).abs() < 1e-5);
    // Half way between two levels the result is their mean, and its overall level is kept.
    mips.bake(0.5, |_| (0.0, 0.0), 1.0, &mut turned);
    assert_eq!(turned.across, mips.levels[0].across);
    let mean = |p: &Pattern| p.cells.iter().sum::<f32>() / p.cells.len() as f32;
    assert!((mean(&turned) - mean(&mips.levels[0])).abs() < 0.02);
    mips.bake(99.0, |_| (0.0, 0.0), 0.5, &mut turned);
    assert_eq!(turned.across, mips.levels[mips.levels.len() - 1].across);
}

#[test]
fn nothing_is_drawn_while_faded_out_or_off_the_frame() {
    let mut hole = Hole::new();
    let mut data = vec![7u8; 64 * 64 * 4];
    let mut frame = Frame::new(&mut data, 64, 64, Blend::Add);
    let mut view = HoleView {
        x: 32.0,
        y: 32.0,
        radius: 6.0,
        normal: Vec3::UP,
        time: 0.0,
        gain: 0.0,
    };
    hole.draw(&mut frame, &view);
    view.gain = 1.0;
    view.x = -900.0;
    hole.draw(&mut frame, &view);
    view.x = f32::NAN;
    hole.draw(&mut frame, &view);
    assert!(data.iter().all(|b| *b == 7));
}
