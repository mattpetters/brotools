# Interchange

Rational-time plans and AAF / DAWproject readers and writers. No UI and no
filesystem access: the engine opens files and resolves linked media.

The AAF structured-storage reader started from Vic Herrera's unmerged
`soundcraft-interchange` (`storytold/soundcraft` pull request 799, Apache-2.0).
It reads composition timelines of source clips and fillers through master mobs
to WAVE sources. This crate extends that reader, and adds a writer for the
same subset, with clip gain, fade in/out, volume and pan automation, markers,
and embedded or linked PCM WAV. Property and class identifiers follow the
public AAF baseline (the same numbers published in the MIT `pyaaf2` model).

DAWproject (Bitwig, MIT) is a zip of `project.xml` plus audio. Times are
seconds. Volume is decibels. Pan is normalized (0 left, 1 right). Clip gain
and fade curves other than the two AAF fade types travel in attributes this
reader understands.

Not implemented: OMF (the usable C library is LGPL) and Pro Tools `.ptx`
(`libptformat` is LGPL-2.1-or-later). This repository does not copy or link
either.
