---
name: flashcards
description: "Spaced repetition flashcard system for learning. SM-2 algorithm, supports text and cloze deletion cards."
version: 1.0.0
metadata:
  echo:
    tags: [Learning, Flashcards, Anki, SRS, Education]
---

# Flashcards

Spaced repetition system using the SM-2 algorithm. SQLite storage.

## Script

```bash
python3 scripts/flashcard_engine.py create-deck "Python基础"
python3 scripts/flashcard_engine.py add "Python基础" "GIL是什么?" "Global Interpreter Lock，全局解释器锁"
python3 scripts/flashcard_engine.py add "Python基础" "list和tuple的区别?" "list可变，tuple不可变" --type basic
python3 scripts/flashcard_engine.py add "Python基础" "Python的GIL是{{Global Interpreter Lock}}" --type cloze
python3 scripts/flashcard_engine.py due                    # show due cards
python3 scripts/flashcard_engine.py review <card_id> 4    # rate 0-5
python3 scripts/flashcard_engine.py stats "Python基础"
python3 scripts/flashcard_engine.py import cards.csv "Python基础"
```

## SM-2 Algorithm

Quality rating (0-5):
- 0-2: Incorrect (reset interval to 1 day)
- 3: Correct but hard (keep interval)
- 4: Correct (normal progression)
- 5: Easy (accelerate interval)

```python
def sm2(quality, repetitions, easiness, interval):
    if quality >= 3:
        if repetitions == 0: interval = 1
        elif repetitions == 1: interval = 6
        else: interval = round(interval * easiness)
        repetitions += 1
    else:
        repetitions = 0
        interval = 1
    easiness = max(1.3, easiness + 0.1 - (5 - quality) * (0.08 + (5 - quality) * 0.02))
    return repetitions, easiness, interval
```

## Card Types

- **Basic**: front → back
- **Cloze**: `Python的GIL是{{Global Interpreter Lock}}` → shows blank
- **Reverse**: auto-generates both directions

## Storage

SQLite at `~/.echo-agent/flashcards.db`:

```sql
CREATE TABLE decks (id INTEGER PRIMARY KEY, name TEXT UNIQUE, created_at TEXT);
CREATE TABLE cards (
    id INTEGER PRIMARY KEY, deck_id INTEGER,
    front TEXT, back TEXT, card_type TEXT DEFAULT 'basic',
    repetitions INTEGER DEFAULT 0, easiness REAL DEFAULT 2.5,
    interval INTEGER DEFAULT 0, next_review TEXT,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);
```

## Channel Integration

Schedule daily review delivery: send due cards as quiz messages through Telegram/WeChat at configured time.

## CSV Import Format

```csv
front,back
What is Python?,A programming language
GIL是什么?,Global Interpreter Lock
```
