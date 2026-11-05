#include <cstdint>
#include <vector>
#include <mapbox/geometry/wagyu/wagyu.hpp>
#include <mapbox/geometry/snap_rounding.hpp>
struct Context {
    mapbox::geometry::multi_polygon<long long> result;
    std::vector<int64_t> output;
};
extern "C" void *vtmake_wagyu_new() noexcept { try { return new Context; } catch (...) { return nullptr; } }
extern "C" void vtmake_wagyu_free(void *p) noexcept { delete static_cast<Context *>(p); }
// Each input triple is op, x, y. Each output triple has the same layout.
extern "C" int vtmake_wagyu_union(void *p, const int64_t *input, size_t n, int scale,
                              const int64_t **output, size_t *count) noexcept {
    try {
        auto &ctx = *static_cast<Context *>(p);
        mapbox::geometry::wagyu::wagyu<long long> engine;
        mapbox::geometry::linear_ring<long long> ring;
        for (size_t i = 0; i < n; ++i) {
            if (input[3*i] != 1) continue;
            size_t j = i+1;
            while (j < n && input[3*j] == 2) ++j;
            if (j >= i+4) {
                ring.clear();
                for (size_t k = i; k < j; ++k)
                    ring.emplace_back(input[3*k+1] * double(scale), input[3*k+2] * double(scale));
                engine.add_ring(ring);
            }
            i = j-1;
        }
        ctx.result.clear();
        engine.execute(mapbox::geometry::wagyu::clip_type_union, ctx.result,
                       mapbox::geometry::wagyu::fill_type_positive,
                       mapbox::geometry::wagyu::fill_type_positive);
        ctx.output.clear();
        for (auto const &poly : ctx.result) for (auto const &r : poly) {
            for (size_t k = 0; k < r.size(); ++k) {
                ctx.output.push_back(k ? 2 : 1);
                ctx.output.push_back(r[k].x);
                ctx.output.push_back(r[k].y);
            }
        }
        *output = ctx.output.data(); *count = ctx.output.size();
        return 0;
    } catch (...) { return -1; }
}

// The snap-round test of tippecanoe geometry.cpp:check_polygon (-a@), a COPY
// of its ring loop. Each input triple is op, x, y. Returns 1 if snap_round
// changes the rings (tippecanoe: "self-intersecting polygon"), 0 if not, -1
// on an exception.
extern "C" int vtmake_snap_round_changes(const int64_t *input, size_t n) noexcept {
    try {
        mapbox::geometry::multi_polygon<long long> mp;
        for (size_t i = 0; i < n; i++) {
            if (input[3*i] == 1) {
                size_t j;
                for (j = i + 1; j < n; j++) {
                    if (input[3*j] != 2) break;
                }
                if (j >= i + 4) {
                    mapbox::geometry::linear_ring<long long> lr;
                    for (size_t k = i; k < j; k++) lr.push_back({input[3*k+1], input[3*k+2]});
                    if (lr.size() >= 3) {
                        mapbox::geometry::polygon<long long> p;
                        p.push_back(std::move(lr));
                        mp.push_back(std::move(p));
                    }
                }
                i = j - 1;
            }
        }
        mapbox::geometry::multi_polygon<long long> mp2 = mapbox::geometry::snap_round(mp, true, true);
        return mp != mp2 ? 1 : 0;
    } catch (...) { return -1; }
}
