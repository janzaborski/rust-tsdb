# Laboratorium 2

- [Wersja skrócona](lab2-Blonowski-Zaborski.pdf) — 11 stron, wersja 1.5.
  [Źródło LaTeX](lab2-Blonowski-Zaborski.tex).
- [Wersja pełna z commita 4bca7d2](lab2-Blonowski-Zaborski-pelna.pdf)
  — 34 strony, wersja 1.4. [Źródło LaTeX](lab2-Blonowski-Zaborski-pelna.tex).
  Oba pliki przywrócono bez zmian z tego commita, do porównania.

Budowanie z katalogu głównego repozytorium:

```bash
latexmk -pdf -interaction=nonstopmode -halt-on-error \
  -outdir=target/lab2-doc \
  pz/lab2/lab2-Blonowski-Zaborski.tex \
  pz/lab2/lab2-Blonowski-Zaborski-pelna.tex
```
