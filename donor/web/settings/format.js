// Общие помощники описаний разделов (settings/*.js): ближайшее из списка значений и «выкл» для нуля.
export const nearest = (list, v) => list.reduce((a, b) => (Math.abs(b - v) < Math.abs(a - v) ? b : a));
export const off = (v, f) => (v ? f(v) : "выкл");
