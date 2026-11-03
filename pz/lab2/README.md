# Laboratorium 2

- [Wersja skrócona](lab2-Blonowski-Zaborski.pdf) — 12 stron, wersja 1.6.
  [Źródło LaTeX](lab2-Blonowski-Zaborski.tex).
- [Wersja pełna](lab2-Blonowski-Zaborski-pelna.pdf)
  — 35 stron, wersja 1.6. [Źródło LaTeX](lab2-Blonowski-Zaborski-pelna.tex).
  Bazuje na wersji z commita `4bca7d2`; nazwy typów, pól, metod i tras HTTP
  w obu wersjach ujednolicono z kodem. Planowane rozszerzenia oznaczono w treści.

Budowanie z katalogu głównego repozytorium:

```bash
latexmk -pdf -interaction=nonstopmode -halt-on-error \
  -outdir=target/lab2-doc \
  pz/lab2/lab2-Blonowski-Zaborski.tex \
  pz/lab2/lab2-Blonowski-Zaborski-pelna.tex
```
