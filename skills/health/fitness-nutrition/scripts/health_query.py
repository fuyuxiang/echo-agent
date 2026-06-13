#!/usr/bin/env python3
"""Health & fitness: exercise DB (wger) + nutrition (USDA)."""

import argparse
import json
import urllib.request
from urllib.parse import quote


def search_exercises(query, language=2):
    url = f"https://wger.de/api/v2/exercise/search/?term={quote(query)}&language={language}&format=json"
    data = json.loads(urllib.request.urlopen(url, timeout=10).read())
    for ex in data.get("suggestions", []):
        d = ex.get("data", {})
        print(f"  {d.get('name', 'N/A')} (id={d.get('id')})")
        if d.get("category"):
            print(f"    Category: {d['category'].get('name', '')}")


def exercise_detail(exercise_id):
    url = f"https://wger.de/api/v2/exerciseinfo/{exercise_id}/?format=json"
    data = json.loads(urllib.request.urlopen(url, timeout=10).read())
    print(f"Name: {data.get('name', 'N/A')}")
    print(f"Category: {data.get('category', {}).get('name', 'N/A')}")
    muscles = [m["name_en"] for m in data.get("muscles", [])]
    if muscles:
        print(f"Muscles: {', '.join(muscles)}")
    desc = data.get("description", "")
    if desc:
        import re
        clean = re.sub(r"<[^>]+>", "", desc)
        print(f"Description: {clean[:300]}")


def search_food(query):
    url = f"https://api.nal.usda.gov/fdc/v1/foods/search?query={quote(query)}&pageSize=5&api_key=DEMO_KEY"
    data = json.loads(urllib.request.urlopen(url, timeout=10).read())
    for food in data.get("foods", []):
        print(f"  {food['description']} ({food.get('brandName', 'Generic')})")
        nutrients = {n["nutrientName"]: n["value"] for n in food.get("foodNutrients", [])[:8]}
        for name, val in nutrients.items():
            print(f"    {name}: {val}")
        print()


def bmi(weight_kg, height_m):
    val = weight_kg / (height_m ** 2)
    if val < 18.5:
        cat = "Underweight"
    elif val < 25:
        cat = "Normal"
    elif val < 30:
        cat = "Overweight"
    else:
        cat = "Obese"
    print(f"BMI: {val:.1f} ({cat})")


def main():
    parser = argparse.ArgumentParser(description="Health & fitness query")
    sub = parser.add_subparsers(dest="cmd")
    sub.add_parser("exercise").add_argument("query")
    sub.add_parser("exercise-detail").add_argument("id", type=int)
    sub.add_parser("food").add_argument("query")
    p = sub.add_parser("bmi")
    p.add_argument("weight", type=float, help="Weight in kg")
    p.add_argument("height", type=float, help="Height in meters")
    args = parser.parse_args()

    if args.cmd == "exercise":
        search_exercises(args.query)
    elif args.cmd == "exercise-detail":
        exercise_detail(args.id)
    elif args.cmd == "food":
        search_food(args.query)
    elif args.cmd == "bmi":
        bmi(args.weight, args.height)
    else:
        parser.print_help()


if __name__ == "__main__":
    main()
