// Arbitrary workflow/config JSON must not pass through JavaScript Number.
// Numeric tokens retain their spelling (including 1.0 and integers above 2^53).
(() => {
  'use strict';
  const numberPattern = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/;
  class ExactNumber {
    constructor(source) {
      if (typeof source !== 'string' || numberPattern.exec(source)?.[0] !== source) throw Error('Invalid JSON number');
      this.source = source;
      Object.freeze(this);
    }
    toString() { return this.source; }
    // Display coercion is allowed; serializing always uses the original token.
    valueOf() { return Number(this.source); }
  }
  function parse(source) {
    if (typeof source !== 'string' || source.length > 4 * 1024 * 1024) throw Error('JSON exceeds 4 MiB');
    let pos = 0;
    const space = () => { while (/[\x20\t\r\n]/.test(source[pos] || '\0')) pos++; };
    const fail = () => { throw Error('Invalid or ambiguous JSON at character ' + pos); };
    function string() {
      const start = pos++;
      while (pos < source.length) {
        const c = source[pos++];
        if (c === '\\') pos++;
        else if (c === '"') return JSON.parse(source.slice(start, pos));
      }
      fail();
    }
    function value(depth) {
      if (depth > 96) throw Error('JSON nesting exceeds 96');
      space();
      const c = source[pos];
      if (c === '"') return string();
      if (c === '{' || c === '[') {
        const object = c === '{', end = object ? '}' : ']';
        const result = object ? Object.create(null) : [];
        pos++; space();
        if (source[pos] === end) { pos++; return result; }
        while (pos < source.length) {
          if (object) {
            if (source[pos] !== '"') fail();
            const key = string(); space();
            if (Object.hasOwn(result, key) || source[pos++] !== ':') fail();
            result[key] = value(depth + 1);
          } else result.push(value(depth + 1));
          space();
          if (source[pos] === end) { pos++; return result; }
          if (source[pos++] !== ',') fail();
          space();
        }
        fail();
      }
      for (const [literal, result] of [['true', true], ['false', false], ['null', null]]) {
        if (source.startsWith(literal, pos)) { pos += literal.length; return result; }
      }
      const number = numberPattern.exec(source.slice(pos));
      if (!number) fail();
      pos += number[0].length;
      return new ExactNumber(number[0]);
    }
    const result = value(0); space();
    if (pos !== source.length) fail();
    return result;
  }
  function stringify(data, pretty = false) {
    const seen = new Set();
    function encode(value, depth) {
      if (depth > 96) throw Error('JSON nesting exceeds 96');
      if (value instanceof ExactNumber) return value.source;
      if (value === null || typeof value === 'string' || typeof value === 'boolean') return JSON.stringify(value);
      if (typeof value === 'number') {
        if (!Number.isFinite(value) || Number.isInteger(value) && !Number.isSafeInteger(value)) throw Error('Use exact JSON for large numbers');
        return JSON.stringify(value);
      }
      if (!value || typeof value !== 'object' || seen.has(value)) throw Error('Only acyclic JSON values are supported');
      seen.add(value);
      const array = Array.isArray(value), entries = array ? value.map(v => encode(v, depth + 1)) :
        Object.keys(value).map(key => JSON.stringify(key) + (pretty ? ': ' : ':') + encode(value[key], depth + 1));
      seen.delete(value);
      const open = array ? '[' : '{', close = array ? ']' : '}';
      return !pretty || !entries.length ? open + entries.join(',') + close :
        open + '\n' + '  '.repeat(depth + 1) + entries.join(',\n' + '  '.repeat(depth + 1)) + '\n' + '  '.repeat(depth) + close;
    }
    return encode(data, 0);
  }
  globalThis.TaskloomJson = Object.freeze({parse, stringify, ExactNumber});
})();
