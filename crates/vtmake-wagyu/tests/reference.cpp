#include "geometry.hpp"
#include <cstdint>
extern "C" size_t bt_reference(const int64_t *data, size_t n, int64_t *out, size_t capacity, bool coalesce) {
    drawvec geom;
    for(size_t i=0;i<n;i++) geom.emplace_back(data[3*i],data[3*i+1],data[3*i+2]);
    if(coalesce) coalesce_polygon(geom,true);
    else geom=clean_or_clip_poly(geom,0,0,false,true);
    if(geom.size()*3>capacity) return geom.size()*3;
    for(size_t i=0;i<geom.size();i++) {out[3*i]=geom[i].op;out[3*i+1]=geom[i].x;out[3*i+2]=geom[i].y;}
    return geom.size()*3;
}
