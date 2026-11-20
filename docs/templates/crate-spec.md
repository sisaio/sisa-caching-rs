# Crate: <crate name>

<!-- Copy this file to docs/specs/<crate>.md, named after the crate's commit scope. The copy sits
in another folder, so check the relative links. The same template is in every sisaio crate
repository. -->

Status: **Draft** | Accepted | Implemented in <version> · Published: yes | no · Label: `area:<scope>` · Commit scope: `<scope>`

## Purpose

One paragraph: what this crate owns and why it exists. Say what it does **not** do.

## Public contract

The main types, traits and features, and what each is for. Sketch a signature where it shows
where a responsibility sits; the code settles the exact one.

## Behaviour

What the crate does, step by step where order matters, with real names from the code and the
default of every setting it reads.

## Guarantees and limits

What a user can rely on, and what they can't. Link the library's guarantees and add only this
crate's own.

## Errors

| Type | Says |
| --- | --- |
| `<Name>Error` | When it is returned |

## Formats

The headers, names and tables this crate reads or writes, as links into `docs/formats.md`. A
format lives there, not here, so other languages find it in one place.

## Open questions

What is still undecided. Remove the section when it is empty.
