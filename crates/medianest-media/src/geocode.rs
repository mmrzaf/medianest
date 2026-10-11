//! Offline reverse geocoding: coordinates to the nearest town and country.
//!
//! Uses the GeoNames table of places with at least 15,000 inhabitants
//! (CC BY 4.0, <https://www.geonames.org/>), embedded in the binary.

use std::collections::HashMap;
use std::sync::OnceLock;

use medianest_core::{GeoPoint, Place};

const PLACES: &str = include_str!("../data/places.tsv");
const COUNTRIES: &str = include_str!("../data/countries.tsv");

/// Within this distance the nearest town names the place.
const CITY_RADIUS_KM: f64 = 40.0;
/// Larger towns this much farther away than the nearest one are preferred,
/// so city districts and neighbourhoods resolve to the city itself.
const PREFER_LARGER_WITHIN_KM: f64 = 10.0;
/// Within this distance at least the country is named.
const COUNTRY_RADIUS_KM: f64 = 250.0;

struct City {
    name: &'static str,
    latitude: f64,
    longitude: f64,
    country: &'static str,
    region: &'static str,
    population: u32,
}

struct Index {
    cities: Vec<City>,
    cells: HashMap<(i32, i32), Vec<u32>>,
    countries: HashMap<&'static str, &'static str>,
}

#[allow(clippy::cast_possible_truncation)]
fn cell(latitude: f64, longitude: f64) -> (i32, i32) {
    (latitude.floor() as i32, longitude.floor() as i32)
}

fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut cities = Vec::with_capacity(35_000);
        for line in PLACES.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split('\t');
            let (Some(name), Some(lat), Some(lon), Some(country), region, population) = (
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
                fields.next(),
            ) else {
                continue;
            };
            let (Ok(latitude), Ok(longitude)) = (lat.parse(), lon.parse()) else {
                continue;
            };
            cities.push(City {
                name,
                latitude,
                longitude,
                country,
                region: region.unwrap_or(""),
                population: population.and_then(|value| value.parse().ok()).unwrap_or(0),
            });
        }
        let mut cells: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
        for (position, city) in cities.iter().enumerate() {
            if let Ok(position) = u32::try_from(position) {
                cells
                    .entry(cell(city.latitude, city.longitude))
                    .or_default()
                    .push(position);
            }
        }
        let countries = COUNTRIES
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| line.split_once('\t'))
            .collect();
        Index {
            cities,
            cells,
            countries,
        }
    })
}

/// Great-circle distance in kilometres.
#[must_use]
pub fn distance_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((lat2 - lat1) / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * ((lon2 - lon1) / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

/// Names the place nearest to a point, or `None` far from any town (at sea).
#[must_use]
pub fn reverse(point: GeoPoint) -> Option<Place> {
    let index = index();
    let (row, column) = cell(point.latitude, point.longitude);
    let here = (point.latitude, point.longitude);
    let mut nearby: Vec<(f64, &City)> = Vec::new();
    for dr in -2..=2i32 {
        for dc in -3..=3i32 {
            let mut wrapped = column + dc;
            if wrapped < -180 {
                wrapped += 360;
            } else if wrapped >= 180 {
                wrapped -= 360;
            }
            let Some(entries) = index.cells.get(&(row + dr, wrapped)) else {
                continue;
            };
            for &entry in entries {
                if let Some(city) = index.cities.get(entry as usize) {
                    nearby.push((distance_km(here, (city.latitude, city.longitude)), city));
                }
            }
        }
    }
    let nearest = nearby
        .iter()
        .map(|(distance, _)| *distance)
        .fold(f64::INFINITY, f64::min);
    let best = nearby
        .iter()
        .filter(|(distance, _)| *distance <= nearest + PREFER_LARGER_WITHIN_KM)
        .max_by(|a, b| {
            a.1.population
                .cmp(&b.1.population)
                .then(b.0.total_cmp(&a.0))
        })
        .map(|(_, city)| (nearest, *city));
    let (distance, city) = best?;
    if distance > COUNTRY_RADIUS_KM {
        return None;
    }
    let country = index
        .countries
        .get(city.country)
        .copied()
        .unwrap_or(city.country);
    Some(Place {
        city: if distance <= CITY_RADIUS_KM {
            city.name.to_owned()
        } else {
            String::new()
        },
        region: (!city.region.is_empty()).then(|| city.region.to_owned()),
        country: country.to_owned(),
        country_code: city.country.to_owned(),
    })
}

/// Country name for an ISO code, as stored in the embedded table.
#[must_use]
pub fn country_name(code: &str) -> Option<&'static str> {
    index().countries.get(code).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(latitude: f64, longitude: f64) -> Option<Place> {
        reverse(GeoPoint::new(latitude, longitude, None).expect("point"))
    }

    #[test]
    fn names_well_known_places() {
        let paris = at(48.8584, 2.2945).expect("paris");
        assert_eq!(
            (paris.city.as_str(), paris.country_code.as_str()),
            ("Paris", "FR")
        );
        assert_eq!(paris.country, "France");
        let tehran = at(35.6997, 51.3380).expect("tehran");
        assert_eq!(tehran.country_code, "IR");
        let sf = at(37.7858, -122.4064).expect("san francisco");
        assert_eq!(sf.city, "San Francisco");
    }

    #[test]
    fn open_ocean_has_no_place() {
        assert!(at(-30.0, -140.0).is_none());
    }

    #[test]
    fn distances_are_sane() {
        let london = (51.5074, -0.1278);
        let paris = (48.8566, 2.3522);
        let km = distance_km(london, paris);
        assert!((340.0..350.0).contains(&km), "{km}");
    }
}
