// Test-only C API to tippecanoe's real geometry code, for the differential
// tests of vtmake-geom (cargo feature `cpp-ref`).
//
// This file includes tile.cpp from the tippecanoe source tree, so that its
// static functions (clip_to_tile, simplify_feature, revive_polygon) are
// visible here. All other tippecanoe files are compiled without change by
// build.rs. Small parts that are copies of tippecanoe code (not calls) are
// marked "COPY".
//
// tippecanoe is BSD-2-Clause:
//
// Copyright (c) 2022, Protomaps LLC
// Copyright (c) 2014, Mapbox Inc.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
//
// - Redistributions of source code must retain the above copyright notice,
//   this list of conditions and the following disclaimer.
// - Redistributions in binary form must reproduce the above copyright notice,
//   this list of conditions and the following disclaimer in the documentation
//   and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.

#include "tile.cpp"
#include "shared_borders.hpp"

#include <cmath>
#include <mutex>
#include <vector>

// Defined in geometry.cpp, not declared in a header.
double label_goodness(const drawvec &dv, long long x, long long y);
drawvec impose_tile_boundaries(const drawvec &geom, long long extent);

// COPY of the struct in main.cpp (same layout), so that the real
// prep_drop_states and calc_feature_minzoom of main.cpp can be called.
struct drop_state {
	double gap;
	unsigned long long previndex;
	double interval;
	double seq;
};
int calc_feature_minzoom(struct index *ix, struct drop_state *ds, int maxzoom, double gamma);
void prep_drop_states(struct drop_state *ds, int maxzoom, int basezoom, double droprate);

struct cdraw {
	long long x;
	long long y;
	int op;
	int necessary;
};

static thread_local std::vector<cdraw> g_out[4];

static drawvec to_dv(const cdraw *g, size_t n) {
	drawvec dv;
	dv.reserve(n);
	for (size_t i = 0; i < n; i++) {
		draw d(g[i].op, g[i].x, g[i].y);
		d.necessary = g[i].necessary;
		dv.push_back(d);
	}
	return dv;
}

static void set_out(int k, const drawvec &dv) {
	g_out[k].clear();
	for (auto const &d : dv) {
		g_out[k].push_back(cdraw{(long long) d.x, (long long) d.y, d.op, d.necessary});
	}
}

extern "C" {

// The flags of the spec command line.
void ref_init() {
	geometry_scale = 0;
	additional[A_DROP_DENSEST_AS_NEEDED] = 1;
	additional[A_EXTEND_ZOOMS] = 1;
	prevent[P_FEATURE_LIMIT] = 1;
	prevent[P_KILOBYTE_LIMIT] = 1;
	encode_index = encode_quadkey;
	decode_index = decode_quadkey;
}

size_t ref_out_len(int k) {
	return g_out[k].size();
}

const cdraw *ref_out_ptr(int k) {
	return g_out[k].data();
}

// ---- numbers and libm ----

long long ref_cvt_i64(double d) {
	return (long long) d;
}
unsigned long long ref_cvt_u64(double d) {
	return (unsigned long long) d;
}
double ref_log(double x) {
	return log(x);
}
double ref_tan(double x) {
	return tan(x);
}
double ref_cos(double x) {
	return cos(x);
}
double ref_sinh(double x) {
	return sinh(x);
}
double ref_atan(double x) {
	return atan(x);
}
double ref_exp(double x) {
	return exp(x);
}

// ---- projection.cpp ----

void ref_lonlat2tile(double lon, double lat, int zoom, long long *x, long long *y) {
	lonlat2tile(lon, lat, zoom, x, y);
}

void ref_tile2lonlat(long long x, long long y, int zoom, double *lon, double *lat) {
	tile2lonlat(x, y, zoom, lon, lat);
}

unsigned long long ref_encode_quadkey(unsigned wx, unsigned wy) {
	return encode_quadkey(wx, wy);
}

void ref_decode_quadkey(unsigned long long ix, unsigned *wx, unsigned *wy) {
	decode_quadkey(ix, wx, wy);
}

unsigned long long ref_bit_reverse(unsigned long long v) {
	return bit_reverse(v);
}

// ---- clip.cpp: area and rings ----

double ref_get_area(const cdraw *g, size_t n, size_t i, size_t j) {
	drawvec dv = to_dv(g, n);
	return get_area(dv, i, j);
}

double ref_get_mp_area(const cdraw *g, size_t n) {
	drawvec dv = to_dv(g, n);
	return get_mp_area(dv);
}

void ref_fix_polygon(const cdraw *g, size_t n) {
	drawvec dv = to_dv(g, n);
	set_out(0, fix_polygon(dv, false, false));
}

// ---- geometry.cpp: center of mass; serial.cpp: index point ----

void ref_center_of_mass_mp(const cdraw *g, size_t n, long long *x, long long *y) {
	drawvec dv = to_dv(g, n);
	draw c = center_of_mass_mp(dv);
	*x = c.x;
	*y = c.y;
}

// COPY of the index point code in serial.cpp:serialize_feature, for the
// spec flags. It calls the real center_of_mass_mp and encode_quadkey.
unsigned long long ref_feature_index(int t, const cdraw *g, size_t n, const long long *bbox) {
	drawvec scaled_geometry = to_dv(g, n);
	long long midx, midy;
	if (t == VT_POINT) {
		midx = (bbox[0] / 2 + bbox[2] / 2) & ((1LL << 32) - 1);
		midy = (bbox[1] / 2 + bbox[3] / 2) & ((1LL << 32) - 1);
	} else if (t == VT_POLYGON) {
		draw scaled_center = center_of_mass_mp(scaled_geometry);
		midx = scaled_center.x & ((1LL << 32) - 1);
		midy = scaled_center.y & ((1LL << 32) - 1);
	} else {
		size_t ix = 0;
		for (size_t i = 0; i < scaled_geometry.size(); i++) {
			ix += scaled_geometry[i].x + scaled_geometry[i].y;
		}
		if (scaled_geometry.size() != 0) {
			ix = ix % scaled_geometry.size();
		}
		midx = scaled_geometry[ix].x & ((1LL << 32) - 1);
		midy = scaled_geometry[ix].y & ((1LL << 32) - 1);
	}
	return encode_quadkey(midx, midy);
}

// ---- clip.cpp / geometry.cpp: clipping ----

int ref_quick_check(const long long *bbox, int z, long long buffer) {
	return quick_check(bbox, z, buffer);
}

int ref_clip(long long *x0, long long *y0, long long *x1, long long *y1, long long xmin, long long ymin, long long xmax, long long ymax) {
	return clip(x0, y0, x1, y1, xmin, ymin, xmax, ymax);
}

void ref_clip_point(const cdraw *g, size_t n, int z, long long buffer) {
	drawvec dv = to_dv(g, n);
	set_out(0, clip_point(dv, z, buffer));
}

void ref_clip_lines(const cdraw *g, size_t n, int z, long long buffer) {
	drawvec dv = to_dv(g, n);
	set_out(0, clip_lines(dv, z, buffer));
}

void ref_simple_clip_poly(const cdraw *g, size_t n, int z, int buffer) {
	drawvec dv = to_dv(g, n);
	drawvec edge_nodes;
	set_out(0, simple_clip_poly(dv, z, buffer, edge_nodes, false));
}

// tile.cpp:clip_to_tile, with the coordinate step of
// geometry.cpp:decode_geometry before it (COPY of that loop: absolute to
// tile-relative, bbox of MOVETO/LINETO). Returns 1 if the feature is not in
// the tile. bbox_out: sf.bbox after the call (for rewrite).
int ref_clip_to_tile(const cdraw *g, size_t n, int t, int z, unsigned tx, unsigned ty, long long buffer, long long *bbox_out) {
	serial_feature sf;
	sf.t = t;
	sf.bbox[0] = LLONG_MAX;
	sf.bbox[1] = LLONG_MAX;
	sf.bbox[2] = LLONG_MIN;
	sf.bbox[3] = LLONG_MIN;
	for (size_t i = 0; i < n; i++) {
		draw d;
		d.op = g[i].op;
		if (d.op == VT_MOVETO || d.op == VT_LINETO) {
			long long wwx = g[i].x;
			long long wwy = g[i].y;
			if (z != 0) {
				wwx -= tx << (32 - z);
				wwy -= ty << (32 - z);
			}
			sf.bbox[0] = std::min(wwx, sf.bbox[0]);
			sf.bbox[1] = std::min(wwy, sf.bbox[1]);
			sf.bbox[2] = std::max(wwx, sf.bbox[2]);
			sf.bbox[3] = std::max(wwy, sf.bbox[3]);
			d.x = wwx;
			d.y = wwy;
		}
		sf.geometry.push_back(d);
	}
	int outside = clip_to_tile(sf, z, buffer) ? 1 : 0;
	for (int k = 0; k < 4; k++) {
		bbox_out[k] = sf.bbox[k];
	}
	set_out(0, sf.geometry);
	return outside;
}

// COPY of the child range part of tile.cpp:rewrite.
void ref_child_range(const long long *bbox, int z, int nextzoom, unsigned tx, unsigned ty, int buffer, unsigned *out4) {
	int span = 1 << (nextzoom - z);
	long long bbox2[4];
	int k;
	for (k = 0; k < 4; k++) {
		bbox2[k] = bbox[k] / (1 << (32 - nextzoom - 8));
	}
	bbox2[0] -= buffer + 1;
	bbox2[1] -= buffer + 1;
	bbox2[2] += buffer;
	bbox2[3] += buffer;
	for (k = 0; k < 4; k++) {
		if (bbox2[k] < 0) {
			bbox2[k] = 0;
		}
		if (bbox2[k] >= 256 * span) {
			bbox2[k] = 256 * (span - 1);
		}
		bbox2[k] /= 256;
	}
	out4[0] = tx * span + bbox2[0];
	out4[1] = ty * span + bbox2[1];
	out4[2] = tx * span + bbox2[2];
	out4[3] = ty * span + bbox2[3];
}

// ---- simplification ----

void ref_remove_noop(const cdraw *g, size_t n, int type, int shift) {
	drawvec dv = to_dv(g, n);
	set_out(0, remove_noop(dv, type, shift));
}

void ref_impose_tile_boundaries(const cdraw *g, size_t n, long long extent) {
	drawvec dv = to_dv(g, n);
	set_out(0, impose_tile_boundaries(dv, extent));
}

double ref_distance_from_line(long long px, long long py, long long ax, long long ay, long long bx, long long by) {
	return distance_from_line(px, py, ax, ay, bx, by);
}

void ref_douglas_peucker(const cdraw *g, size_t n, int start, int count, double e, size_t kept, size_t retain) {
	drawvec dv = to_dv(g, n);
	douglas_peucker(dv, start, count, e, kept, retain, false);
	set_out(0, dv);
}

void ref_simplify_lines(const cdraw *g, size_t n, int z, int detail, int mark_tile_bounds, double simplification, size_t retain) {
	drawvec dv = to_dv(g, n);
	drawvec shared_nodes;
	set_out(0, simplify_lines(dv, z, 0, 0, detail, mark_tile_bounds, simplification, retain, shared_nodes, NULL, 0, ""));
}

// tile.cpp:simplify_feature (static). Returns the area.
double ref_simplify_feature(const cdraw *g, size_t n, int t, int z, int detail, double simplification, int reduced) {
	serial_feature sf;
	sf.geometry = to_dv(g, n);
	sf.t = t;
	sf.z = z;
	sf.tx = 0;
	sf.ty = 0;
	sf.line_detail = detail;
	sf.extra_detail = detail;
	sf.maxzoom = 18;
	sf.reduced = reduced;
	sf.coalesced = false;
	sf.simplification = simplification;
	drawvec shared_nodes;
	double area = simplify_feature(&sf, shared_nodes, NULL, 0, "");
	set_out(0, sf.geometry);
	return area;
}

void ref_to_tile_scale(const cdraw *g, size_t n, int z, int detail) {
	drawvec dv = to_dv(g, n);
	to_tile_scale(dv, z, detail);
	set_out(0, dv);
}

void ref_close_poly(const cdraw *g, size_t n) {
	drawvec dv = to_dv(g, n);
	set_out(0, close_poly(dv));
}

// ---- tiny polygons ----

void ref_reduce_tiny_poly(const cdraw *g, size_t n, int z, int detail, double tiny, double *accum, int *still, int *reduced) {
	drawvec dv = to_dv(g, n);
	bool s = false, r = false;
	set_out(0, reduce_tiny_poly(dv, z, detail, &s, &r, accum, tiny));
	*still = s;
	*reduced = r;
}

int ref_line_is_too_small(const cdraw *g, size_t n, int z, int detail) {
	drawvec dv = to_dv(g, n);
	return line_is_too_small(dv, z, detail);
}

void ref_revive_polygon(const cdraw *g, size_t n, double area, int z, int detail) {
	drawvec dv = to_dv(g, n);
	set_out(0, revive_polygon(dv, area, z, detail));
}

void ref_coalesce_polygon(const cdraw *g, size_t n, int scale_up) {
	drawvec dv = to_dv(g, n);
	coalesce_polygon(dv, scale_up);
	set_out(0, dv);
}

// The per-feature steps of tile.cpp:write_tile and simplification_worker
// after next_feature, for one feature (COPY of the order of the calls; each
// step calls the real function). Input: the clipped, tile-relative geometry.
//
// Returns 0 if the feature is dropped before simplification, else 1.
// Outputs: 0 = tile-unit geometry before cleaning, 1 = cleaned (polygons),
// 2 = final geometry (after revive, close_poly and the encode remove_noop).
int ref_tile_steps(const cdraw *g, size_t n, int t, int z, int detail, double simplification, double tiny, double *accum, int *tiny_counted) {
	serial_feature sf;
	sf.geometry = to_dv(g, n);
	sf.t = t;
	*tiny_counted = 0;
	for (int k = 0; k < 3; k++) {
		g_out[k].clear();
	}

	int line_detail = detail;
	bool still_need_simplification_after_reduction = false;
	if (sf.t == VT_POLYGON) {
		bool simplified_away_by_reduction = false;
		sf.geometry = reduce_tiny_poly(sf.geometry, z, line_detail, &still_need_simplification_after_reduction, &simplified_away_by_reduction, accum, tiny);
		if (simplified_away_by_reduction) {
			*tiny_counted = 1;
		}
		if (sf.geometry.size() == 0) {
			return 0;
		}
	} else {
		still_need_simplification_after_reduction = true;
	}

	if (sf.t == VT_POLYGON || sf.t == VT_LINE) {
		if (line_is_too_small(sf.geometry, z, line_detail)) {
			return 0;
		}
	}

	sf.reduced = !still_need_simplification_after_reduction;
	sf.coalesced = false;
	sf.z = z;
	sf.tx = 0;
	sf.ty = 0;
	sf.line_detail = line_detail;
	sf.extra_detail = line_detail;
	sf.maxzoom = 18;
	sf.simplification = simplification;

	// simplification_worker
	drawvec shared_nodes;
	double area = simplify_feature(&sf, shared_nodes, NULL, 0, "");
	drawvec geom = sf.geometry;
	to_tile_scale(geom, z, sf.extra_detail);
	set_out(0, geom);

	if (t == VT_POLYGON) {
		drawvec before = geom;
		coalesce_polygon(geom, true);
		set_out(1, geom);
		if (geom.size() < 3) {
			if (area > 0) {
				geom = revive_polygon(before, area, z, sf.extra_detail);
			} else {
				geom.clear();
			}
		}
		// write_tile: close_poly for polygons
		geom = close_poly(geom);
	}

	// encode: remove_noop for lines and polygons
	if (t == VT_LINE || t == VT_POLYGON) {
		geom = remove_noop(geom, t, 0);
	}
	set_out(2, geom);
	return 1;
}

// ---- main.cpp: drop rate ----

void *ref_drop_new(int maxzoom, int basezoom, double droprate) {
	drop_state *ds = new drop_state[maxzoom + 1];
	prep_drop_states(ds, maxzoom, basezoom, droprate);
	return ds;
}

int ref_drop_calc(void *ds, int t, int maxzoom) {
	struct index ix;
	ix.t = t;
	return calc_feature_minzoom(&ix, (drop_state *) ds, maxzoom, 0);
}

void ref_drop_free(void *ds) {
	delete[] (drop_state *) ds;
}
}

// ---- the geometry options ----
//
// tippecanoe keeps its options in the global arrays prevent[] and
// additional[]. The functions below set them for one call, under a lock, and
// then set them back to the spec flags.

static std::mutex g_flags;

struct ref_geom_opts {
	int maxzoom;
	int no_simplify;	  // -ps
	int no_simplify_maxzoom;  // -pS
	int grid_low_zooms;	  // -aL
	int visvalingam;	  // -av
	int no_clipping;	  // -pc
	int no_duplication;	  // -pD
	int shared_nodes;	  // -pn
	int shared_borders;	  // -ab
};

static void set_flags(const ref_geom_opts *o) {
	prevent[P_SIMPLIFY] = o->no_simplify;
	prevent[P_SIMPLIFY_LOW] = o->no_simplify_maxzoom;
	additional[A_GRID_LOW_ZOOMS] = o->grid_low_zooms;
	additional[A_VISVALINGAM] = o->visvalingam;
	prevent[P_CLIPPING] = o->no_clipping;
	prevent[P_DUPLICATION] = o->no_duplication;
	prevent[P_SIMPLIFY_SHARED_NODES] = o->shared_nodes;
	additional[A_DETECT_SHARED_BORDERS] = o->shared_borders;
}

static void clear_flags() {
	ref_geom_opts o = {};
	set_flags(&o);
}

extern "C" {

void ref_visvalingam(const cdraw *g, size_t n, size_t start, size_t end, double threshold, size_t retain) {
	drawvec dv = to_dv(g, n);
	visvalingam(dv, start, end, threshold, retain);
	set_out(0, dv);
}

void ref_stairstep(const cdraw *g, size_t n, int z, int detail) {
	drawvec dv = to_dv(g, n);
	set_out(0, stairstep(dv, z, detail));
}

int ref_pnpoly(const cdraw *g, size_t n, size_t start, size_t nvert, long long x, long long y) {
	drawvec dv = to_dv(g, n);
	return pnpoly(dv, start, nvert, x, y);
}

// geometry.cpp:check_polygon: output 0 is the geometry after the call (its
// messages go to standard error).
void ref_check_polygon(const cdraw *g, size_t n) {
	drawvec dv = to_dv(g, n);
	check_polygon(dv);
	set_out(0, dv);
}

void ref_simplify_lines_opts(const cdraw *g, size_t n, int z, int detail, int mark_tile_bounds, double simplification, size_t retain, const ref_geom_opts *o) {
	std::lock_guard<std::mutex> lock(g_flags);
	set_flags(o);
	drawvec dv = to_dv(g, n);
	drawvec shared_nodes;
	set_out(0, simplify_lines(dv, z, 0, 0, detail, mark_tile_bounds, simplification, retain, shared_nodes, NULL, 0, ""));
	clear_flags();
}

double ref_simplify_feature_opts(const cdraw *g, size_t n, int t, int z, int detail, double simplification, int reduced, const ref_geom_opts *o) {
	std::lock_guard<std::mutex> lock(g_flags);
	set_flags(o);
	serial_feature sf;
	sf.geometry = to_dv(g, n);
	sf.t = t;
	sf.z = z;
	sf.tx = 0;
	sf.ty = 0;
	sf.line_detail = detail;
	sf.extra_detail = detail;
	sf.maxzoom = o->maxzoom;
	sf.reduced = reduced;
	sf.coalesced = false;
	sf.simplification = simplification;
	drawvec shared_nodes;
	double area = simplify_feature(&sf, shared_nodes, NULL, 0, "");
	set_out(0, sf.geometry);
	clear_flags();
	return area;
}

// As ref_clip_to_tile (COPY of its decode loop), with the flags of o.
// Output 1: sf.edge_nodes.
int ref_clip_to_tile_opts(const cdraw *g, size_t n, int t, int z, unsigned tx, unsigned ty, long long buffer, long long *bbox_out, const ref_geom_opts *o) {
	std::lock_guard<std::mutex> lock(g_flags);
	set_flags(o);
	serial_feature sf;
	sf.t = t;
	sf.bbox[0] = LLONG_MAX;
	sf.bbox[1] = LLONG_MAX;
	sf.bbox[2] = LLONG_MIN;
	sf.bbox[3] = LLONG_MIN;
	for (size_t i = 0; i < n; i++) {
		draw d;
		d.op = g[i].op;
		if (d.op == VT_MOVETO || d.op == VT_LINETO) {
			long long wwx = g[i].x;
			long long wwy = g[i].y;
			if (z != 0) {
				wwx -= tx << (32 - z);
				wwy -= ty << (32 - z);
			}
			sf.bbox[0] = std::min(wwx, sf.bbox[0]);
			sf.bbox[1] = std::min(wwy, sf.bbox[1]);
			sf.bbox[2] = std::max(wwx, sf.bbox[2]);
			sf.bbox[3] = std::max(wwy, sf.bbox[3]);
			d.x = wwx;
			d.y = wwy;
		}
		sf.geometry.push_back(d);
	}
	int outside = clip_to_tile(sf, z, buffer) ? 1 : 0;
	for (int k = 0; k < 4; k++) {
		bbox_out[k] = sf.bbox[k];
	}
	set_out(0, sf.geometry);
	set_out(1, sf.edge_nodes);
	clear_flags();
	return outside;
}

// simplify_lines with -pn: the tile nodes and the global node keys (sorted).
void ref_simplify_lines_nodes(const cdraw *g, size_t n, int z, int tx, int ty, int detail, int mark_tile_bounds, double simplification, size_t retain, const cdraw *tile, size_t ntile, const unsigned long long *global, size_t nglobal, const ref_geom_opts *o) {
	std::lock_guard<std::mutex> lock(g_flags);
	set_flags(o);
	drawvec dv = to_dv(g, n);
	drawvec shared_nodes = to_dv(tile, ntile);
	std::vector<node> map(nglobal);
	for (size_t i = 0; i < nglobal; i++) {
		map[i].index = global[i];
	}
	std::string bloom(1, (char) 0xFF);
	set_out(0, simplify_lines(dv, z, tx, ty, detail, mark_tile_bounds, simplification, retain, shared_nodes, map.data(), nglobal * sizeof(node), bloom));
	clear_flags();
}

// shared_borders.cpp:find_common_edges (merge fraction 1) for polygons.
// Input and output 0: the polygons, each followed by a draw with op 99.
void ref_find_common_edges(const cdraw *g, size_t n, int z, int detail, double simplification, const ref_geom_opts *o) {
	std::lock_guard<std::mutex> lock(g_flags);
	set_flags(o);
	std::vector<std::shared_ptr<serial_feature>> features;
	drawvec cur;
	for (size_t i = 0; i < n; i++) {
		if (g[i].op == 99) {
			auto f = std::make_shared<serial_feature>();
			f->t = VT_POLYGON;
			f->geometry = cur;
			f->renamed = -1;
			f->index = 0;
			features.push_back(f);
			cur.clear();
		} else {
			draw d(g[i].op, g[i].x, g[i].y);
			cur.push_back(d);
		}
	}
	find_common_edges(features, z, detail, simplification, o->maxzoom, 1);
	drawvec out;
	for (auto &f : features) {
		for (auto &d : f->geometry) {
			out.push_back(d);
		}
		out.push_back(draw(99, 0, 0));
	}
	set_out(0, out);
	clear_flags();
}

double ref_label_goodness(const cdraw *g, size_t n, long long x, long long y) {
	drawvec dv = to_dv(g, n);
	return label_goodness(dv, x, y);
}

void ref_polygon_to_anchor(const cdraw *g, size_t n) {
	drawvec dv = to_dv(g, n);
	set_out(0, polygon_to_anchor(dv));
}

void ref_checkerboard_anchors(const cdraw *g, size_t n, int tx, int ty, int z, unsigned long long label_point) {
	drawvec dv = to_dv(g, n);
	set_out(0, checkerboard_anchors(dv, tx, ty, z, label_point));
}

}  // extern "C"
