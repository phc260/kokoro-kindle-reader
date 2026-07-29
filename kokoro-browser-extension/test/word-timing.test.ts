// Word timing is an estimate, so what is worth testing is the shape of it, not the numbers.
//
// Kokoro hands back PCM with no alignment (see src/word-timing.ts), so these marks are the only
// thing the highlight has to move on. They must start at zero, never go backwards, and fit
// inside the chunk's real duration - a mark past the end would leave the highlight parked on a
// word whose audio finished, one chunk behind, for the rest of the page.

import { test, expect } from 'bun:test';
import { scheduleWords, syllables, wordSpans } from '../src/word-timing';

test('syllables tracks how long a word takes to say', () => {
  expect(syllables('the')).toBe(1);
  expect(syllables('whale')).toBe(1); // silent e
  expect(syllables('greenery')).toBe(3);
  expect(syllables('water')).toBe(2);
  // Same length, very different durations - which is the whole reason this is not a character
  // count.
  expect(syllables('greenery')).toBeGreaterThan(syllables('thoughts'));
});

test('digits are spoken, not spelled', () => {
  // No vowel groups at all, so a letters-only rule scores this 1 and the highlight sprints
  // through a date.
  expect(syllables('1997')).toBeGreaterThan(3);
});

test('a word is charged for the pause that follows it', () => {
  const [plain] = wordSpans('word');
  const [clause] = wordSpans('word,');
  const [sentence] = wordSpans('word.');
  const [quoted] = wordSpans('word."'); // the mark is under the closing quote
  expect(clause!.weight).toBeGreaterThan(plain!.weight);
  expect(sentence!.weight).toBeGreaterThan(clause!.weight);
  expect(quoted!.weight).toBe(sentence!.weight);
});

test('spans point at the words of the chunk they came from', () => {
  const text = 'Call me Ishmael.';
  for (const s of wordSpans(text)) {
    expect(text.slice(s.charIndex, s.charIndex + s.charLength)).toMatch(/^\S+$/);
  }
  expect(wordSpans(text)).toHaveLength(3);
});

test('marks start at zero, never go backwards, and fit inside the audio', () => {
  const text = 'Call me Ishmael. Some years ago, never mind how long precisely, having little money.';
  const duration = 6.4;
  const marks = scheduleWords(text, duration);

  expect(marks).toHaveLength(wordSpans(text).length);
  expect(marks[0]!.at).toBe(0);
  for (let i = 1; i < marks.length; i++) {
    expect(marks[i]!.at).toBeGreaterThanOrEqual(marks[i - 1]!.at);
    expect(marks[i]!.charIndex).toBeGreaterThan(marks[i - 1]!.charIndex);
  }
  expect(marks[marks.length - 1]!.at).toBeLessThan(duration);
});

test('nothing to say and nothing to say it in are both empty, not NaN', () => {
  expect(scheduleWords('', 5)).toEqual([]);
  expect(scheduleWords('   \n ', 5)).toEqual([]);
  // A punctuation-only chunk synthesizes to no samples at all.
  expect(scheduleWords('Hello there.', 0)).toEqual([]);
});
