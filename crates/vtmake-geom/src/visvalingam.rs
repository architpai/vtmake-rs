//! Visvalingam simplification (`-av`): a port of tippecanoe
//! `visvalingam.cpp`, with the same heap and the same tie order.
//!
//! tippecanoe adapted this code from
//! https://github.com/paulmach/orb/blob/dcade4901baea0727377ccf7c4aab2addd92d152/simplify/visvalingam.go
//!
//! The MIT License (MIT)
//!
//! Copyright (c) 2017 Paul Mach
//!
//! Permission is hereby granted, free of charge, to any person obtaining
//! a copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject
//! to the following conditions:
//!
//! The above copyright notice and this permission notice shall be
//! included in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
//! EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
//! MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
//! IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR
//! ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
//! CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
//!
//! The tippecanoe parts are BSD-2-Clause (see LICENSE-tippecanoe.md).

use vtmake_core::Draw;

const NONE: usize = usize::MAX;

#[derive(Clone, Copy)]
struct Item {
    area: f64,
    point: usize,
    next: usize,
    previous: usize,
    index: usize,
}

/// Item slot 0 is the list start (`linkedListStart`); slot k (k >= 1) is
/// the point k of the part.
struct Heap<'a> {
    h: Vec<usize>,
    items: &'a mut [Item],
}

impl Heap<'_> {
    fn push(&mut self, it: usize) {
        self.items[it].index = self.h.len();
        self.h.push(it);
        self.up(self.items[it].index);
    }

    fn pop(&mut self) -> usize {
        let removed = self.h[0];
        let last = self.h.pop().unwrap();
        if !self.h.is_empty() {
            self.items[last].index = 0;
            self.h[0] = last;
            self.down(0);
        }
        removed
    }

    fn update(&mut self, it: usize, area: f64) {
        if self.items[it].area > area {
            self.items[it].area = area;
            self.up(self.items[it].index);
        } else {
            self.items[it].area = area;
            self.down(self.items[it].index);
        }
    }

    fn up(&mut self, mut i: usize) {
        let object = self.h[i];
        while i > 0 {
            let up = ((i + 1) >> 1) - 1;
            let parent = self.h[up];
            if self.items[parent].area <= self.items[object].area {
                break;
            }
            self.items[parent].index = i;
            self.h[i] = parent;
            self.items[object].index = up;
            self.h[up] = object;
            i = up;
        }
    }

    fn down(&mut self, mut i: usize) {
        let object = self.h[i];
        loop {
            let right = (i + 1) << 1;
            let left = right - 1;
            let mut down = i;
            let mut child = self.h[down];
            if left < self.h.len() && self.items[self.h[left]].area < self.items[child].area {
                down = left;
                child = self.h[down];
            }
            if right < self.h.len() && self.items[self.h[right]].area < self.items[child].area {
                down = right;
                child = self.h[down];
            }
            if down == i {
                break;
            }
            self.items[child].index = i;
            self.h[i] = child;
            self.items[object].index = down;
            self.h[down] = object;
            i = down;
        }
    }
}

/// `doubleTriangleArea`: the products are in 64-bit integers (they wrap as
/// in the C++ code), then `std::abs` and the conversion to `double`.
#[inline]
fn double_triangle_area(ls: &[Draw], start: usize, i1: usize, i2: usize, i3: usize) -> f64 {
    let (a, b, c) = (ls[i1 + start], ls[i2 + start], ls[i3 + start]);
    let v = (b.x.wrapping_sub(a.x))
        .wrapping_mul(c.y.wrapping_sub(a.y))
        .wrapping_sub((b.y.wrapping_sub(a.y)).wrapping_mul(c.x.wrapping_sub(a.x)));
    v.wrapping_abs() as f64
}

/// `visvalingam(ls, start, end, threshold, retain)`: mark the points of
/// `ls[start..end]` that the simplification keeps (`necessary`). It does
/// not clear other marks. Requires `end - start >= 2`.
pub fn visvalingam(ls: &mut [Draw], start: usize, end: usize, threshold: f64, retain: usize) {
    let n = end - start;
    let threshold = threshold * 2.0;
    let mut removed = 0usize;
    // Slot 0: the list start. Slots 1..n: the points 1..n-1 (slot k = point k).
    let blank = Item { area: 0.0, point: 0, next: NONE, previous: NONE, index: 0 };
    let mut items = vec![blank; n];
    items[0].area = f64::INFINITY;
    let mut heap = Heap { h: Vec::with_capacity(n), items: &mut items };
    heap.push(0);
    {
        let mut previous = 0usize;
        for i in 1..n - 1 {
            let area = double_triangle_area(ls, start, i - 1, i, i + 1);
            let it = &mut heap.items[i];
            it.area = area;
            it.point = i;
            it.previous = previous;
            heap.push(i);
            heap.items[previous].next = i;
            previous = i;
        }
        let e = n - 1;
        heap.items[e].area = f64::INFINITY;
        heap.items[e].point = n - 1;
        heap.items[e].previous = previous;
        heap.items[previous].next = e;
        heap.push(e);
    }

    while !heap.h.is_empty() {
        let current = heap.pop();
        let cur = heap.items[current];
        if cur.area > threshold || n - removed <= retain {
            break;
        }
        let next = cur.next;
        let previous = cur.previous;
        heap.items[previous].next = cur.next;
        heap.items[next].previous = cur.previous;
        removed += 1;

        let pp = heap.items[previous].previous;
        if pp != NONE {
            let area = double_triangle_area(ls, start, heap.items[pp].point, heap.items[previous].point, heap.items[next].point);
            heap.update(previous, area.max(cur.area));
        }
        let nn = heap.items[next].next;
        if nn != NONE {
            let area = double_triangle_area(ls, start, heap.items[previous].point, heap.items[next].point, heap.items[nn].point);
            heap.update(next, area.max(cur.area));
        }
    }

    let mut it = 0usize;
    while it != NONE {
        ls[heap.items[it].point + start].necessary = true;
        it = heap.items[it].next;
    }
}
