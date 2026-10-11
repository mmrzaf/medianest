#!/usr/bin/env python3
"""Regenerates the offline place-name tables from GeoNames.

Downloads cities15000.zip, admin1CodesASCII.txt and countryInfo.txt from
https://download.geonames.org/export/dump/ (CC BY 4.0) into a temporary
directory and writes compact tables to crates/medianest-media/data/.
"""
import csv
import io
import os
import sys
import tempfile
import urllib.request
import zipfile

BASE = "https://download.geonames.org/export/dump/"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "crates", "medianest-media", "data")


def fetch(name, directory):
    path = os.path.join(directory, name)
    if not os.path.exists(path):
        urllib.request.urlretrieve(BASE + name, path)
    return path


def main(directory):
    countries = {}
    with open(fetch("countryInfo.txt", directory), encoding="utf-8") as handle:
        for line in handle:
            if line.startswith("#"):
                continue
            parts = line.rstrip("\n").split("\t")
            if len(parts) > 4:
                countries[parts[0]] = parts[4]
    regions = {}
    with open(fetch("admin1CodesASCII.txt", directory), encoding="utf-8") as handle:
        for line in handle:
            code, name, _ascii, _id = line.rstrip("\n").split("\t")
            regions[code] = name
    rows = []
    with zipfile.ZipFile(fetch("cities15000.zip", directory)) as archive:
        with archive.open("cities15000.txt") as raw:
            for line in io.TextIOWrapper(raw, encoding="utf-8"):
                parts = line.rstrip("\n").split("\t")
                name, lat, lon, code, cc, admin1, population = (
                    parts[1], parts[4], parts[5], parts[7], parts[8], parts[10], parts[14] or "0"
                )
                # Skip city districts and historical or abandoned places.
                if cc not in countries or code in {"PPLX", "PPLH", "PPLQ", "PPLW", "PPLCH"}:
                    continue
                region = regions.get(f"{cc}.{admin1}", "")
                rows.append((name, f"{float(lat):.4f}", f"{float(lon):.4f}", cc, region, population))
    rows.sort(key=lambda row: (row[3], row[0]))
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "places.tsv"), "w", encoding="utf-8", newline="") as handle:
        handle.write("# GeoNames cities15000, CC BY 4.0, https://www.geonames.org/\n")
        writer = csv.writer(handle, delimiter="\t", lineterminator="\n", quoting=csv.QUOTE_NONE, escapechar="\\")
        writer.writerows(rows)
    with open(os.path.join(OUT, "countries.tsv"), "w", encoding="utf-8", newline="") as handle:
        handle.write("# GeoNames countryInfo, CC BY 4.0, https://www.geonames.org/\n")
        for code in sorted(countries):
            handle.write(f"{code}\t{countries[code]}\n")
    print(f"{len(rows)} places, {len(countries)} countries")


if __name__ == "__main__":
    if len(sys.argv) > 1:
        main(sys.argv[1])
    else:
        with tempfile.TemporaryDirectory() as tmp:
            main(tmp)
