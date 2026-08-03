# Шаблон страницы

> Это шаблон для новой wiki-страницы. Он показывает обязательную информацию,
> а не описывает отдельную runtime capability.

```markdown
# Название возможности

> **Статус:** Experimental | Stable | Planned | Research  
> **Crate / module:** `yuyib::...`  
> **Платформы:** Windows / platform-neutral / backend-dependent  
> **Requires:** feature, plugin или dependency (если есть)

Одно предложение: когда использовать и какой результат получить.

## Быстрый пример

Короткий компилируемый пример **с пояснением, зачем каждая функция**, или
ссылка на canonical example + tutorial.

Для beginner path см. также раздел **Учебные tutorials** в SUMMARY.

## Модель / lifecycle

Кто создаёт объект, кто им владеет, когда он валиден и где допустимо вызывать API.

## API

Ссылка на rustdoc и таблица ключевых entry points. Не дублировать все signatures.

## Limits & Caveats

- hard limits / defaults / backend-dependent constraints;
- behaviour при ошибке или превышении budget;
- performance и memory cost;
- platform-specific differences и production workaround.

## См. также

Смежные guide, global topic, API reference и example.
```

Для каждой реализованной capability обязательны **Статус**, **crate/module** и
**Limits & Caveats**. Planned page не может выглядеть как готовый API: её
пример должен быть marked pseudocode или отсутствовать.
