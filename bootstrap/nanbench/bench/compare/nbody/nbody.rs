//! The energy of the Sun and the giant planets before and after the size's steps of a simulation of their orbits.

use std::f64::consts::PI;

/// The number the first argument states in decimal.
fn size() -> u64 {
    let arg = std::env::args().nth(1).expect("a size argument");
    arg.parse().expect("a size in decimal")
}

const SOLAR_MASS: f64 = 4.0 * PI * PI;

const DAYS_PER_YEAR: f64 = 365.24;

/// A body's position, velocity and mass.
#[derive(Clone, Copy)]
struct Body {
    x: f64,
    y: f64,
    z: f64,
    vx: f64,
    vy: f64,
    vz: f64,
    mass: f64,
}

/// The Sun and the giant planets, the Sun at rest at the origin.
fn system() -> [Body; 5] {
    let mut bodies = [Body {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        vx: 0.0,
        vy: 0.0,
        vz: 0.0,
        mass: SOLAR_MASS,
    }; 5];
    bodies[1] = Body {
        x: 4.84143144246472090,
        y: -1.16032004402742839,
        z: -0.103622044471123109,
        vx: 0.00166007664274403694 * DAYS_PER_YEAR,
        vy: 0.00769901118419740425 * DAYS_PER_YEAR,
        vz: -0.0000690460016972063023 * DAYS_PER_YEAR,
        mass: 0.000954791938424326609 * SOLAR_MASS,
    };
    bodies[2] = Body {
        x: 8.34336671824457987,
        y: 4.12479856412430479,
        z: -0.403523417114321381,
        vx: -0.00276742510726862411 * DAYS_PER_YEAR,
        vy: 0.00499852801234917238 * DAYS_PER_YEAR,
        vz: 0.0000230417297573763929 * DAYS_PER_YEAR,
        mass: 0.000285885980666130812 * SOLAR_MASS,
    };
    bodies[3] = Body {
        x: 12.8943695621391310,
        y: -15.1111514016986312,
        z: -0.223307578892655734,
        vx: 0.00296460137564761618 * DAYS_PER_YEAR,
        vy: 0.00237847173959480950 * DAYS_PER_YEAR,
        vz: -0.0000296589568540237556 * DAYS_PER_YEAR,
        mass: 0.0000436624404335156298 * SOLAR_MASS,
    };
    bodies[4] = Body {
        x: 15.3796971148509165,
        y: -25.9193146099879641,
        z: 0.179258772950371181,
        vx: 0.00268067772490389322 * DAYS_PER_YEAR,
        vy: 0.00162824170038242295 * DAYS_PER_YEAR,
        vz: -0.0000951592254519715870 * DAYS_PER_YEAR,
        mass: 0.0000515138902046611451 * SOLAR_MASS,
    };
    bodies
}

/// Gives the Sun the velocity that makes the system's momentum zero.
fn offset_momentum(bodies: &mut [Body; 5]) {
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for body in bodies.iter() {
        px += body.vx * body.mass;
        py += body.vy * body.mass;
        pz += body.vz * body.mass;
    }
    bodies[0].vx = -px / SOLAR_MASS;
    bodies[0].vy = -py / SOLAR_MASS;
    bodies[0].vz = -pz / SOLAR_MASS;
}

/// The system's kinetic plus potential energy.
fn energy(bodies: &[Body; 5]) -> f64 {
    let mut total = 0.0;
    for i in 0..5 {
        let body = bodies[i];
        total += 0.5 * body.mass * (body.vx * body.vx + body.vy * body.vy + body.vz * body.vz);
        for j in (i + 1)..5 {
            let other = bodies[j];
            let dx = body.x - other.x;
            let dy = body.y - other.y;
            let dz = body.z - other.z;
            total -= body.mass * other.mass / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    total
}

/// Moves the bodies by one step of `dt`.
fn advance(bodies: &mut [Body; 5], dt: f64) {
    for i in 0..5 {
        for j in (i + 1)..5 {
            let dx = bodies[i].x - bodies[j].x;
            let dy = bodies[i].y - bodies[j].y;
            let dz = bodies[i].z - bodies[j].z;
            let distance = (dx * dx + dy * dy + dz * dz).sqrt();
            let magnitude = dt / (distance * distance * distance);
            let mass_i = bodies[i].mass;
            let mass_j = bodies[j].mass;
            bodies[i].vx -= dx * mass_j * magnitude;
            bodies[i].vy -= dy * mass_j * magnitude;
            bodies[i].vz -= dz * mass_j * magnitude;
            bodies[j].vx += dx * mass_i * magnitude;
            bodies[j].vy += dy * mass_i * magnitude;
            bodies[j].vz += dz * mass_i * magnitude;
        }
    }
    for body in bodies.iter_mut() {
        body.x += dt * body.vx;
        body.y += dt * body.vy;
        body.z += dt * body.vz;
    }
}

fn main() {
    let n = size();
    let mut bodies = system();
    offset_momentum(&mut bodies);
    let before = (energy(&bodies) * 1_000_000_000.0).round_ties_even() as i64;
    for _ in 0..n {
        advance(&mut bodies, 0.01);
    }
    let after = (energy(&bodies) * 1_000_000_000.0).round_ties_even() as i64;
    println!("{before} {after}");
}
