# Tagging and Filtering Plan

## Detection Priority Order (Check in this order, stop at first match)

### 1. **Messages** (Highest Priority - Check First)
**Primary Indicator: Message Bubbles**
- 3+ short lines (< 120 chars each) → Messages
- 2+ short lines + name prefix ("John:", "You:", etc.) → Messages
- 2+ short lines + timestamp on line → Messages
- 2+ name prefixes → Messages
- 2+ timestamps on lines → Messages

**Secondary Indicators:**
- Any timestamp (12h or 24h format)
- Message app names (iMessage, Slack, Discord, etc.)
- Read receipts (read, delivered, sent, seen)
- Chat words (lmao, lol, haha, etc.) + questions
- Questions + casual greetings
- Date headers (Today, Yesterday) + timestamp

### 2. **Code**
- Code keywords (function, const, let, var, class, import, export, def, return, async, await, fn, impl, struct)
- Code symbols ({, }, =>, ->, ::, ())
- Indentation patterns
- Comments (//, /*, #)

### 3. **Design**
- Hex colors (#RRGGBB)
- Design tools (Figma, Sketch, Adobe, etc.)
- Design terms (px, rem, font, color, background, border, padding, margin) + "design"

### 4. **Receipts**
- Prices ($X.XX) + receipt words (total, subtotal, tax, receipt, invoice, paid, order)
- Prices + dates

### 5. **Browser**
- URLs (http://, https://, www.)
- Browser UI elements (address bar, bookmarks, back, forward, etc.)
- Navigation elements (←, →, ↻, ⌂)
- Multiple domains + URLs

### 6. **Terminal**
- Prompts ($ , ~ , > )
- Commands (cd, ls, git, npm, cargo, python, node)

### 7. **Errors**
- Error words (error, exception, failed, panic, segfault, undefined, traceback, stack trace)
- Stack traces

### 8. **Documents** (Only if NOT Messages)
- 50+ words
- Structured content (paragraphs, sentences)
- Document patterns (chapter, section, article, document, page, heading, title, author)
- Lists (bullets, numbered)
- Formal language (therefore, however, in conclusion)
- **MUST NOT have message indicators**

### 9. **Images** (Fallback)
- Empty or < 10 chars text → Images
- Minimal text (< 50 chars, < 10 words) + no other tags
- UI overlay text (screenshot, image, photo, camera, gallery)
- OCR noise

## Rules

1. **Messages take priority** - If message bubbles detected, tag as Messages immediately
2. **Documents are strict** - Only tag as Documents if clearly NOT a message
3. **One primary tag** - Each screenshot gets one primary tag (first match wins)
4. **Empty text = Images** - Screenshots with no/minimal text are Images
