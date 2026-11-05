//! Compare the stream parser with the reference parser in place.
use vtmake_parse::{ParsedLine, parse_feature, stream::StreamParser};
use rayon::prelude::*;
fn main() {
    for path in std::env::args().skip(1) {
        let file = std::fs::File::open(&path).unwrap();
        let map = unsafe { memmap2::Mmap::map(&file) }.unwrap();
        let (lines, fast) = map
            .par_split(|&b| b == b'\n')
            .map_init(
                || (StreamParser::default(), ParsedLine::default()),
                |(stream, reference), line| {
                    let accepted = stream.parse(line);
                    let result = parse_feature(line, reference);
                    if accepted {
                        result.unwrap();
                        assert!(reference.warnings.is_empty());
                        assert_eq!(reference.features.len(), 1);
                        let expected = &reference.features[0];
                        let actual = &stream.feature;
                        assert_eq!(actual.geom_type, expected.geom_type);
                        assert_eq!(actual.minzoom, expected.minzoom);
                        assert_eq!(actual.maxzoom, expected.maxzoom);
                        assert_eq!(actual.layer, expected.layer);
                        assert_eq!(actual.id, expected.id);
                        assert_eq!(actual.geom.len(), expected.geom.len());
                        for (a, b) in actual.geom.iter().zip(&expected.geom) {
                            assert_eq!(
                                (a.op, a.lon.to_bits(), a.lat.to_bits()),
                                (b.op, b.lon.to_bits(), b.lat.to_bits())
                            );
                        }
                        assert_eq!(stream.attrs.len(), expected.attrs.len());
                        for (a, b) in stream.attrs.iter().zip(&expected.attrs) {
                            assert_eq!(
                                (a.key.as_ref(), a.ty, a.value.as_ref()),
                                (b.key.as_ref(), b.ty, b.value.as_ref())
                            );
                        }
                    }
                    (1u64, u64::from(accepted))
                },
            )
            .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        println!(
            "{path}: {lines} lines, {fast} stream, {} reference, 0 mismatches",
            lines - fast
        );
    }
}
