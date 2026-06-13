#!/usr/bin/env python3
"""Geo query: POI search, geocoding, routing via Nominatim + OSRM."""

import argparse
import json
import urllib.request
from urllib.parse import quote

NOMINATIM = "https://nominatim.openstreetmap.org"
OSRM = "https://router.project-osrm.org"
HEADERS = {"User-Agent": "EchoAgent/1.0"}


def _get(url):
    req = urllib.request.Request(url, headers=HEADERS)
    return json.loads(urllib.request.urlopen(req, timeout=15).read())


def geocode(query):
    data = _get(f"{NOMINATIM}/search?q={quote(query)}&format=json&limit=5")
    for r in data:
        print(f"  {r['display_name']}")
        print(f"    lat={r['lat']}, lon={r['lon']}")
    return data


def reverse_geocode(lat, lon):
    data = _get(f"{NOMINATIM}/reverse?lat={lat}&lon={lon}&format=json")
    print(data.get("display_name", "Unknown"))
    return data


def poi_search(query, lat=None, lon=None):
    url = f"{NOMINATIM}/search?q={quote(query)}&format=json&limit=10"
    if lat and lon:
        url += f"&viewbox={lon-0.1},{lat+0.1},{lon+0.1},{lat-0.1}&bounded=1"
    data = _get(url)
    for r in data:
        print(f"  {r['display_name']}")
    return data


def route(from_coords, to_coords, mode="driving"):
    url = f"{OSRM}/route/v1/{mode}/{from_coords[1]},{from_coords[0]};{to_coords[1]},{to_coords[0]}?overview=false"
    data = _get(url)
    if data.get("routes"):
        r = data["routes"][0]
        dist_km = r["distance"] / 1000
        dur_min = r["duration"] / 60
        print(f"Distance: {dist_km:.1f} km")
        print(f"Duration: {dur_min:.0f} min")
    return data


def main():
    parser = argparse.ArgumentParser(description="Maps & POI query")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("geocode").add_argument("query")
    p = sub.add_parser("reverse")
    p.add_argument("lat", type=float)
    p.add_argument("lon", type=float)
    p = sub.add_parser("poi")
    p.add_argument("query")
    p.add_argument("--lat", type=float)
    p.add_argument("--lon", type=float)
    p = sub.add_parser("route")
    p.add_argument("from_lat", type=float)
    p.add_argument("from_lon", type=float)
    p.add_argument("to_lat", type=float)
    p.add_argument("to_lon", type=float)
    args = parser.parse_args()

    if args.cmd == "geocode":
        geocode(args.query)
    elif args.cmd == "reverse":
        reverse_geocode(args.lat, args.lon)
    elif args.cmd == "poi":
        poi_search(args.query, args.lat, args.lon)
    elif args.cmd == "route":
        route((args.from_lat, args.from_lon), (args.to_lat, args.to_lon))
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
