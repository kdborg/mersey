// CLI twin of `bench/cli/mersey/csv.mersey`. No JS runtime has a CSV builtin,
// so `std/csv.mersey`'s parser and serializer are ported here line for line —
// what is compared is the two engines running one program.
const QUOTE = 34; // "
const COMMA = 44; // ,
const LF = 10; //    \n
const CR = 13; //    \r
const DQ = '"';

function parse(text) {
  const rows = [];
  let row = [];
  let field = "";
  let inQuotes = false;
  let started = false;
  let i = 0;
  while (i < text.length) {
    const cp = text.codePointAt(i);
    const c = text.charAt(i);
    if (inQuotes) {
      if (cp === QUOTE) {
        if (i + 1 < text.length && text.codePointAt(i + 1) === QUOTE) {
          field = `${field}${c}`;
          i += 2;
          continue;
        }
        inQuotes = false;
        i += 1;
        continue;
      }
      field = `${field}${c}`;
      i += 1;
      continue;
    }
    if (cp === QUOTE) {
      inQuotes = true;
      started = true;
      i += 1;
      continue;
    }
    if (cp === COMMA) {
      row.push(field);
      field = "";
      started = true;
      i += 1;
      continue;
    }
    if (cp === LF || cp === CR) {
      row.push(field);
      field = "";
      rows.push(row);
      row = [];
      started = false;
      if (cp === CR && i + 1 < text.length && text.codePointAt(i + 1) === LF) {
        i += 2;
      } else {
        i += 1;
      }
      continue;
    }
    field = `${field}${c}`;
    started = true;
    i += 1;
  }
  if (started || field.length > 0 || row.length > 0) {
    row.push(field);
    rows.push(row);
  }
  return rows;
}

function doubleQuotes(s) {
  let out = "";
  for (let i = 0; i < s.length; i += 1) {
    const c = s.charAt(i);
    if (s.codePointAt(i) === QUOTE) {
      out = `${out}${DQ}${DQ}`;
    } else {
      out = `${out}${c}`;
    }
  }
  return out;
}

function quoteField(s) {
  let needs = false;
  let hasQuote = false;
  for (let i = 0; i < s.length; i += 1) {
    const cp = s.codePointAt(i);
    if (cp === COMMA || cp === LF || cp === CR) {
      needs = true;
    } else if (cp === QUOTE) {
      needs = true;
      hasQuote = true;
    }
  }
  if (!needs) {
    return s;
  }
  const body = hasQuote ? doubleQuotes(s) : s;
  return `${DQ}${body}${DQ}`;
}

function stringify(rows) {
  const lines = [];
  for (const row of rows) {
    const cells = [];
    for (const field of row) {
      cells.push(quoteField(field));
    }
    lines.push(cells.join(","));
  }
  return lines.join("\r\n");
}

function source(rows) {
  let out = "id,name,note,qty\r\n";
  for (let i = 0; i < rows; i += 1) {
    const name = `item ${i}`;
    const note = i % 3 === 0
      ? `"quoted, with comma"`
      : (i % 3 === 1 ? `say ""hi"" there` : `plain note ${i}`);
    out = `${out}${i},${name},"${note}",${i * 7}\r\n`;
  }
  return out;
}

function work(n, text) {
  let sum = 0;
  for (let r = 0; r < n; r += 1) {
    const grid = parse(text);
    sum = (sum + grid.length) % 1000003;
    for (let i = 0; i < grid.length; i += 1) {
      const row = grid[i];
      sum = (sum + row.length) % 1000003;
      for (let j = 0; j < row.length; j += 1) {
        sum = (sum * 31 + row[j].length) % 1000003;
      }
    }
    const back = stringify(grid);
    sum = (sum + back.length) % 1000003;
  }
  return sum;
}

const text = source(60);
work(5, text); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(300, text);
const t1 = performance.now();
console.log(`RESULT csv ${t1 - t0} ${c}`);
