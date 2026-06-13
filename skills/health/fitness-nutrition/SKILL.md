---
name: fitness-nutrition
description: "Search exercises by muscle/equipment, lookup food nutrition data. Uses free wger and USDA APIs."
version: 1.0.0
metadata:
  echo:
    tags: [Fitness, Nutrition, Health, Exercise, Diet]
---

# Fitness & Nutrition

Exercise database and food nutrition lookup via free public APIs.

## Exercise Search (wger API — free, no key)

```bash
# Search exercises
curl -s "https://wger.de/api/v2/exercise/search/?term=squat&language=english&format=json"

# List muscle groups
curl -s "https://wger.de/api/v2/muscle/?format=json" | python3 -c "
import sys,json; [print(f\"{m['id']}: {m['name_en']}\") for m in json.load(sys.stdin)['results']]"

# Exercises for specific muscle (e.g. chest=4)
curl -s "https://wger.de/api/v2/exercise/?muscles=4&language=2&format=json"
```

## Muscle Groups

| ID | English | 中文 |
|----|---------|------|
| 1 | Biceps | 肱二头肌 |
| 2 | Shoulders | 肩部 |
| 4 | Chest | 胸部 |
| 9 | Legs | 腿部 |
| 10 | Abs/Core | 核心/腹肌 |
| 12 | Back | 背部 |
| 5 | Triceps | 肱三头肌 |

## Nutrition Lookup (USDA FoodData Central)

```bash
# Search food (free, DEMO_KEY works)
curl -s "https://api.nal.usda.gov/fdc/v1/foods/search?query=chicken+breast&pageSize=3&api_key=DEMO_KEY" | \
  python3 -c "import sys,json; [print(f\"{f['description']}: {[n for n in f.get('foodNutrients',[]) if 'Energy' in n.get('nutrientName','')]}\") for f in json.load(sys.stdin)['foods']]"
```

## Calculators

```python
# BMI
bmi = weight_kg / (height_m ** 2)
# Categories: <18.5 underweight, 18.5-24.9 normal, 25-29.9 overweight, 30+ obese

# TDEE (Harris-Benedict)
bmr_male = 88.362 + (13.397 * weight_kg) + (4.799 * height_cm) - (5.677 * age)
bmr_female = 447.593 + (9.247 * weight_kg) + (3.098 * height_cm) - (4.330 * age)
# Activity multiplier: sedentary 1.2, light 1.375, moderate 1.55, active 1.725
```

## Script

```bash
python3 scripts/health_query.py exercise "push up"
python3 scripts/health_query.py exercise-detail 123
python3 scripts/health_query.py food "鸡胸肉"
python3 scripts/health_query.py bmi 75 1.78
```
