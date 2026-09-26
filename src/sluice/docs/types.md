# Types

Function inputs/outputs and plan inputs are typed. Types use CWL spellings and are compared by
structure, not by name.

| Type | Meaning |
|---|---|
| `string`, `int`, `float`, `boolean`, `Any` | primitives; `Any` accepts anything |
| `T?` (e.g. `string?`, `string[]?`) | optional: T or null; an optional input may be left unbound |
| `["null", T]` | optional form for complex T, e.g. an optional enum |
| `T[]` (e.g. `string[]`) | array of T |
| `{"type": "array", "items": T}` | array of any T |
| `{"type": "enum", "symbols": ["a", "b"]}` | one of these strings |
| `{"type": "record", "fields": {"f": T}}` | object with these fields |

An output fits an input when: either is `Any`; same primitive, or `int` into `float`; an enum
into a `string` or a larger enum; arrays of fitting items; a record that has every required field
of the input record with fitting types (extra fields are fine). An optional value does not fit a
required input.
