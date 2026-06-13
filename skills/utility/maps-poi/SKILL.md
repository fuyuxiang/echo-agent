---
name: maps-poi
description: "Geocoding, POI search, route planning, and timezone queries via OpenStreetMap/Nominatim. Free, no API key."
version: 1.0.0
metadata:
  echo:
    tags: [Maps, Location, POI, Route, Geocoding, Utility]
---

# Maps & POI

Geographic queries using free OpenStreetMap APIs.

## Geocoding (Nominatim)

```bash
# Forward: name → coordinates
curl -s "https://nominatim.openstreetmap.org/search?q=Beijing&format=json&limit=3" \
  -H "User-Agent: EchoAgent/1.0"

# Reverse: coordinates → address
curl -s "https://nominatim.openstreetmap.org/reverse?lat=39.9042&lon=116.4074&format=json" \
  -H "User-Agent: EchoAgent/1.0"
```

## POI Search

```bash
# Search near location
curl -s "https://nominatim.openstreetmap.org/search?q=coffee+near+Chaoyang+Beijing&format=json&limit=10" \
  -H "User-Agent: EchoAgent/1.0"
```

## Route Planning (OSRM)

```bash
# Driving route: Beijing → Shanghai
curl -s "https://router.project-osrm.org/route/v1/driving/116.4,39.9;121.5,31.2?overview=false" | \
  python3 -c "import sys,json; r=json.load(sys.stdin)['routes'][0]; print(f\"Distance: {r['distance']/1000:.1f} km, Duration: {r['duration']/3600:.1f} h\")"
```

## Timezone

```python
from zoneinfo import ZoneInfo
from datetime import datetime

# Get timezone name from coordinates
# pip install timezonefinder
from timezonefinder import TimezoneFinder
tf = TimezoneFinder()
tz_name = tf.timezone_at(lng=116.4, lat=39.9)  # 'Asia/Shanghai'

# Convert time
beijing_time = datetime.now(ZoneInfo("Asia/Shanghai"))
ny_time = beijing_time.astimezone(ZoneInfo("America/New_York"))
```

## Script

```bash
python3 scripts/geo_query.py geocode "天安门"
python3 scripts/geo_query.py reverse 39.9042 116.4074
python3 scripts/geo_query.py poi "咖啡" --lat 39.9 --lon 116.4
python3 scripts/geo_query.py route 39.9042 116.4074 31.2304 121.4737
```

## Rate Limits

Nominatim: max 1 request/second. Always include `User-Agent` header.

## Alternative: 高德地图 (AMap)

For China-specific use, set `AMAP_API_KEY`:
```bash
curl "https://restapi.amap.com/v3/geocode/geo?key=$AMAP_API_KEY&address=北京天安门"
```
