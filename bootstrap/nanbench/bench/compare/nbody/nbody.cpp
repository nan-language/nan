// The energy of the Sun and the giant planets before and after the size's steps of a simulation of their orbits.

#include <array>
#include <cmath>
#include <cstdint>
#include <iostream>
#include <numbers>
#include <stdexcept>
#include <string>

// The number the first argument states in decimal.
static std::uint64_t size(int argc, char** argv) {
    if (argc < 2) {
        throw std::invalid_argument("a size argument");
    }
    return std::stoull(argv[1]);
}

constexpr double SOLAR_MASS = 4.0 * std::numbers::pi * std::numbers::pi;

constexpr double DAYS_PER_YEAR = 365.24;

// A body's position, velocity and mass.
struct Body {
    double x;
    double y;
    double z;
    double vx;
    double vy;
    double vz;
    double mass;
};

using Bodies = std::array<Body, 5>;

// The Sun and the giant planets, the Sun at rest at the origin.
static Bodies system() {
    Bodies bodies;
    bodies.fill(Body{0.0, 0.0, 0.0, 0.0, 0.0, 0.0, SOLAR_MASS});
    bodies[1] = Body{4.84143144246472090, -1.16032004402742839, -0.103622044471123109, 0.00166007664274403694 * DAYS_PER_YEAR, 0.00769901118419740425 * DAYS_PER_YEAR, -0.0000690460016972063023 * DAYS_PER_YEAR, 0.000954791938424326609 * SOLAR_MASS};
    bodies[2] = Body{8.34336671824457987, 4.12479856412430479, -0.403523417114321381, -0.00276742510726862411 * DAYS_PER_YEAR, 0.00499852801234917238 * DAYS_PER_YEAR, 0.0000230417297573763929 * DAYS_PER_YEAR, 0.000285885980666130812 * SOLAR_MASS};
    bodies[3] = Body{12.8943695621391310, -15.1111514016986312, -0.223307578892655734, 0.00296460137564761618 * DAYS_PER_YEAR, 0.00237847173959480950 * DAYS_PER_YEAR, -0.0000296589568540237556 * DAYS_PER_YEAR, 0.0000436624404335156298 * SOLAR_MASS};
    bodies[4] = Body{15.3796971148509165, -25.9193146099879641, 0.179258772950371181, 0.00268067772490389322 * DAYS_PER_YEAR, 0.00162824170038242295 * DAYS_PER_YEAR, -0.0000951592254519715870 * DAYS_PER_YEAR, 0.0000515138902046611451 * SOLAR_MASS};
    return bodies;
}

// Gives the Sun the velocity that makes the system's momentum zero.
static void offset_momentum(Bodies& bodies) {
    double px = 0.0;
    double py = 0.0;
    double pz = 0.0;
    for (const Body& body : bodies) {
        px += body.vx * body.mass;
        py += body.vy * body.mass;
        pz += body.vz * body.mass;
    }
    bodies[0].vx = -px / SOLAR_MASS;
    bodies[0].vy = -py / SOLAR_MASS;
    bodies[0].vz = -pz / SOLAR_MASS;
}

// The system's kinetic plus potential energy.
static double energy(const Bodies& bodies) {
    double total = 0.0;
    for (std::size_t i = 0; i < 5; ++i) {
        Body body = bodies[i];
        total += 0.5 * body.mass * (body.vx * body.vx + body.vy * body.vy + body.vz * body.vz);
        for (std::size_t j = i + 1; j < 5; ++j) {
            Body other = bodies[j];
            double dx = body.x - other.x;
            double dy = body.y - other.y;
            double dz = body.z - other.z;
            total -= body.mass * other.mass / std::sqrt(dx * dx + dy * dy + dz * dz);
        }
    }
    return total;
}

// Moves the bodies by one step of `dt`.
static void advance(Bodies& bodies, double dt) {
    for (std::size_t i = 0; i < 5; ++i) {
        for (std::size_t j = i + 1; j < 5; ++j) {
            double dx = bodies[i].x - bodies[j].x;
            double dy = bodies[i].y - bodies[j].y;
            double dz = bodies[i].z - bodies[j].z;
            double distance = std::sqrt(dx * dx + dy * dy + dz * dz);
            double magnitude = dt / (distance * distance * distance);
            double mass_i = bodies[i].mass;
            double mass_j = bodies[j].mass;
            bodies[i].vx -= dx * mass_j * magnitude;
            bodies[i].vy -= dy * mass_j * magnitude;
            bodies[i].vz -= dz * mass_j * magnitude;
            bodies[j].vx += dx * mass_i * magnitude;
            bodies[j].vy += dy * mass_i * magnitude;
            bodies[j].vz += dz * mass_i * magnitude;
        }
    }
    for (Body& body : bodies) {
        body.x += dt * body.vx;
        body.y += dt * body.vy;
        body.z += dt * body.vz;
    }
}

int main(int argc, char** argv) {
    std::uint64_t n = size(argc, argv);
    Bodies bodies = system();
    offset_momentum(bodies);
    long long before = std::llrint(energy(bodies) * 1000000000.0);
    for (std::uint64_t step = 0; step < n; ++step) {
        advance(bodies, 0.01);
    }
    long long after = std::llrint(energy(bodies) * 1000000000.0);
    std::cout << before << ' ' << after << '\n';
}
