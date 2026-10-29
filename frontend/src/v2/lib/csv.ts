// CSV downloads: the Desk's history and the admin's user reports.

export type CsvValue = string | number | null | undefined;

function csvCell(v: CsvValue): string {
  const s = v == null ? '' : String(v);
  return /[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

/** `rows` (the header first) as a CSV file named `filename`. */
export function downloadCsv(filename: string, rows: CsvValue[][]) {
  const csv = rows.map((r) => r.map(csvCell).join(',')).join('\n');
  const url = URL.createObjectURL(new Blob([csv], { type: 'text/csv' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  a.click();
  URL.revokeObjectURL(url);
}
