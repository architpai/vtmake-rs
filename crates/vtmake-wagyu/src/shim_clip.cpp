// The clip variant of the polygon union (vtmake build --postfilter). It is in
// its own file: a second caller of the Wagyu templates in shim.cpp changes how
// the compiler inlines them in the usual path.
#include <cstdint>
#include <vector>
#include <mapbox/geometry/wagyu/wagyu.hpp>
struct Context {
    mapbox::geometry::multi_polygon<long long> result;
    std::vector<int64_t> output;
};
// A COPY of vtmake_wagyu_union at scale 1, with the clip ring of tippecanoe
// clean_or_clip_poly (clip.cpp) from lo to hi (the postfilter of vtmake
// build). It is a separate function, so that the usual path stays the same.
extern "C" int vtmake_wagyu_union_clip(void *p, const int64_t *input, size_t n, int64_t lo, int64_t hi,
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
                    ring.emplace_back(input[3*k+1], input[3*k+2]);
                engine.add_ring(ring);
            }
            i = j-1;
        }
        ring.clear();
        ring.emplace_back(lo, lo);
        ring.emplace_back(lo, hi);
        ring.emplace_back(hi, hi);
        ring.emplace_back(hi, lo);
        ring.emplace_back(lo, lo);
        engine.add_ring(ring, mapbox::geometry::wagyu::polygon_type_clip);
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
