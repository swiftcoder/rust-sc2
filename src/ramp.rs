use crate::{bot::Rs, geometry::Point2, pixel_map::ByteMap};
use std::{
	cmp::{Ordering, Reverse},
	convert::TryInto,
	fmt,
};

#[derive(Default)]
pub struct Ramps {
	pub all: Vec<Ramp>,
	pub my: Ramp,
	pub enemy: Ramp,
}

type Pos = (usize, usize);

#[derive(Default, Clone)]
pub struct Ramp {
	pub points: Vec<Pos>,
	height: Rs<ByteMap>,
	start_location: Point2,
}
impl Ramp {
	pub fn new(points: Vec<Pos>, height: &Rs<ByteMap>, start_location: Point2) -> Self {
		Self {
			points,
			height: Rs::clone(&height),
			start_location,
			/*points: points
			.into_iter()
			.map(|(x, y)| Point2::new(x as f32, y as f32))
			.collect(),*/
			// cache: Default::default(),
			// cache2: Default::default(),
		}
	}
	pub fn upper(&self) -> Vec<Pos> {
		let mut max = u8::MIN;
		let mut result = Vec::new();

		self.points.iter().for_each(|&p| {
			let h = self.height[p];
			match h.cmp(&max) {
				Ordering::Greater => {
					max = h;
					result = vec![p];
				}
				Ordering::Equal => result.push(p),
				_ => {}
			}
		});

		result
	}
	pub fn lower(&self) -> Vec<Pos> {
		let mut min = u8::MAX;
		let mut result = Vec::new();

		self.points.iter().for_each(|&p| {
			let h = self.height[p];
			match h.cmp(&min) {
				Ordering::Less => {
					min = h;
					result = vec![p];
				}
				Ordering::Equal => result.push(p),
				_ => {}
			}
		});

		result
	}
	pub(crate) fn top_center(&self) -> Option<Pos> {
		let ps = self.upper();
		if ps.is_empty() {
			None
		} else {
			// Some(ps.iter().sum::<Point2>() / ps.len())
			let (x, y) = ps.iter().fold((0, 0), |(ax, ay), (x, y)| (ax + x, ay + y));
			Some((x / ps.len(), y / ps.len()))
		}
	}
	fn bottom_center(&self) -> Option<Pos> {
		let ps = self.lower();
		if ps.is_empty() {
			None
		} else {
			let (x, y) = ps.iter().fold((0, 0), |(ax, ay), (x, y)| (ax + x, ay + y));
			Some((x / ps.len(), y / ps.len()))
		}
	}
	fn upper2_for_ramp_wall(&self) -> Option<[Pos; 2]> {
		let mut upper = self.upper();
		if upper.len() > 5 {
			return None;
		}
		match upper.len().cmp(&2) {
			Ordering::Greater => self.bottom_center().and_then(|(center_x, center_y)| {
				upper.sort_unstable_by_key(|(x, y)| Reverse(center_x * x + center_y * y));
				upper[..2].try_into().ok()
			}),
			Ordering::Equal => upper.as_slice().try_into().ok(),
			Ordering::Less => None,
		}
	}
	pub fn barracks_in_middle(&self) -> Option<Point2> {
		let upper_len = self.upper().len();
		if upper_len != 2 && upper_len != 5 {
			return None;
		}
		if let Some(ps) = self.upper2_for_ramp_wall() {
			let (x, y) = ps[0];
			let p1 = Point2::new(x as f32 + 0.5, y as f32 + 0.5);
			let (x, y) = ps[1];
			let p2 = Point2::new(x as f32 + 0.5, y as f32 + 0.5);

			let intersects = p1.circle_intersection(p2, 2.236_068); // 5_f32.sqrt()
			let (x, y) = *self.lower().first().unwrap();
			let lower = Point2::new(x as f32, y as f32);

			return intersects
				.iter()
				.max_by(|i1, i2| {
					i1.distance_squared(lower)
						.partial_cmp(&i2.distance_squared(lower))
						.unwrap()
				})
				.copied();
		}
		None
	}
	pub fn depot_in_middle(&self) -> Option<Point2> {
		let upper_len = self.upper().len();
		if upper_len != 2 && upper_len != 5 {
			return None;
		}
		if let Some(ps) = self.upper2_for_ramp_wall() {
			let (x, y) = ps[0];
			let p1 = Point2::new(x as f32 + 0.5, y as f32 + 0.5);
			let (x, y) = ps[1];
			let p2 = Point2::new(x as f32 + 0.5, y as f32 + 0.5);

			let intersects = p1.circle_intersection(p2, 1.581_138_8); // 2.5_f32.sqrt()
			let (x, y) = *self.lower().first().unwrap();
			let lower = Point2::new(x as f32, y as f32);

			return intersects
				.iter()
				.max_by(|i1, i2| {
					i1.distance_squared(lower)
						.partial_cmp(&i2.distance_squared(lower))
						.unwrap()
				})
				.copied();
		}
		None
	}
	pub fn corner_depots(&self) -> Option<[Point2; 2]> {
		if let Some(ps) = self.upper2_for_ramp_wall() {
			let (x, y) = ps[0];
			let p1 = Point2::new(x as f32 + 0.5, y as f32 + 0.5);
			let (x, y) = ps[1];
			let p2 = Point2::new(x as f32 + 0.5, y as f32 + 0.5);

			let center = (p1 + p2) / 2.0;

			return Some(center.circle_intersection(self.depot_in_middle()?, 2.236_068)); // 5_f32.sqrt()
		}
		None
	}
	pub fn barracks_correct_placement(&self) -> Option<Point2> {
		self.barracks_in_middle().map(|pos| {
			if self
				.corner_depots()
				.map_or(false, |depots| pos.x + 1.0 > depots[0].x.max(depots[1].x))
			{
				pos
			} else {
				pos.offset(-2.0, 0.0)
			}
		})
	}
	pub fn protoss_wall_pylon(&self) -> Option<Point2> {
		let middle = self.depot_in_middle()?;
		Some(middle + (self.barracks_in_middle()? - middle) * 6.0)
	}
	pub fn protoss_wall_buildings(&self) -> Option<[Point2; 2]> {
		let middle = self.depot_in_middle()?;
		let direction = self.barracks_in_middle()? - middle;

		let mut depots = self.corner_depots()?.to_vec();
		let start = self.start_location;
		depots.sort_unstable_by(|d1, d2| {
			d1.distance_squared(start)
				.partial_cmp(&d2.distance_squared(start))
				.unwrap()
		});

		let wall1 = depots[1] + direction;
		Some([wall1, middle + direction + (middle - wall1) / 1.5])
	}
	pub fn protoss_wall_warpin(&self) -> Option<Point2> {
		let middle = self.depot_in_middle()?;
		let direction = self.barracks_in_middle()? - middle;

		let mut depots = self.corner_depots()?.to_vec();
		let start = self.start_location;
		depots.sort_unstable_by(|d1, d2| {
			d1.distance_squared(start)
				.partial_cmp(&d2.distance_squared(start))
				.unwrap()
		});

		Some(depots[0] - direction)
	}
}
impl fmt::Debug for Ramp {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "Ramp({:?})", self.points)
	}
}