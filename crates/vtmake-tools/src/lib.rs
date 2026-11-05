//! `vtmake join`, `decode`, `enumerate`, `overzoom` and `json-tool`: ports of
//! tippecanoe's `tile-join`, `tippecanoe-decode`, `tippecanoe-enumerate`,
//! `tippecanoe-overzoom` and `tippecanoe-json-tool` (felt/tippecanoe 4f26211).
//!
//! Ported from tippecanoe. Copyright (c) 2022, Protomaps LLC; Copyright (c)
//! 2014, Mapbox Inc. BSD 2-Clause licence: see LICENSE-tippecanoe.md.
//!
//! The output is byte-identical to the felt tools: the same JSON text from
//! `decode`, and the same tiles and metadata from `join` (only `generator`
//! names vtmake). See `docs/tools.md` for the supported options.

pub mod decode;
pub mod enumerate;
pub mod join;
pub mod jsontool;
pub mod overzoom;

mod common;
pub mod getopt;
mod json;
pub mod jsonw;
mod mvtdec;
mod usage;
