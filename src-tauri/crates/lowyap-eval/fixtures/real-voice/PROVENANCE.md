# Real-Voice Global Accent & Multilingual Corpus Provenance (`SV-98`)

Every speech clip in `fixtures/real-voice/` is an authentic **real-human voice recording** sourced from permissively licensed open speech corpora (`CC-BY-4.0` and `CC0-1.0`) and normalized via `ffmpeg -ac 1 -ar 16000 -sample_fmt s16 -bitexact` to 16 kHz mono 16-bit PCM. Zero synthetic TTS (`say`, `AVSpeechSynthesizer`, neural TTS, or sine waves) is used for speech.

- **Total clips:** 18 (16 real-human speech clips + 2 `silence_breath` room-tone/breath probes)
- **Total audio duration:** 137.28 s
- **Total WAV size:** 4.19 MiB (4,393,592 bytes, `< 10 MB` budget)

## Upstream Dataset Attributions & Licenses

1. **LibriSpeech ASR Corpus (`openslr/librispeech_asr`, `test-clean`)** — Vassil Panayotov, Guoguo Chen, Daniel Povey, Sanjeev Khudanpur (2015). Licensed under **CC-BY-4.0** (`https://creativecommons.org/licenses/by/4.0/`). Source: `https://www.openslr.org/12` / `https://huggingface.co/datasets/openslr/librispeech_asr`.
2. **Google FLEURS (`google/fleurs`, `en_us`, `hi_in`, `ur_pk`, `es_419`, `de_de` `test` splits)** — Alexis Conneau et al. (2022), *FLEURS: Few-shot Learning Evaluation of Universal Representations of Speech*. Licensed under **CC-BY-4.0** (`https://creativecommons.org/licenses/by/4.0/`). Source: `https://huggingface.co/datasets/google/fleurs`.
3. **GLOBE / Mozilla Common Voice Corpus (`MushanW/GLOBE`, `test` split)** — Wenbin Wang et al. (2024), curated from Mozilla Common Voice crowdsourced human recordings. Dedicated to the Public Domain under **CC0-1.0** (`https://creativecommons.org/publicdomain/zero/1.0/`). Source: `https://huggingface.co/datasets/MushanW/GLOBE`.
4. **Silence & Breath Hallucination Probes (`silence_room_tone_01.wav`, `silence_breath_puff_02.wav`)** — Deterministic non-speech `-50 dBFS` ambient room tone and soft low-RMS breath puff (`reference_text` omitted) used strictly to verify `0` Whisper phantom hallucinations on silence.

## Clip-by-Clip Provenance & SHA-256 Checksums

| Clip ID | WAV File | Category | Lang | Dur (s) | Dataset & Source ID | Speaker / Locale | License | SHA-256 |
|---|---|---|---|---:|---|---|---|---|
| `en_us_clean_01` | `en_us_clean_01.wav` | `clean_english` | `en` | 5.03 | openslr/librispeech_asr (test-clean) (`row=2, id=6930-75918-0002, speaker=6930`) | US / General English | `CC-BY-4.0` | `ca418189ed1db4ce1bf5e6940d51a9ce870d4d52f3ef9019ccdbb2189d1a0a9c` |
| `en_us_clean_02` | `en_us_clean_02.wav` | `clean_english` | `en` | 7.46 | openslr/librispeech_asr (test-clean) (`row=122, id=1320-122612-0002, speaker=1320`) | US / General English | `CC-BY-4.0` | `096812d54a8d3c4c5723a98bae12c783e087c98a407bc8094dcf2be1eb0cc3ad` |
| `en_us_fleurs_03` | `en_us_fleurs_03.wav` | `clean_english` | `en` | 8.76 | google/fleurs (en_us/test) (`fleurs_id=1675, file=10052240106321793346.wav`) | US English (MALE) | `CC-BY-4.0` | `d903ba75c0509b98e53f5b376be08a77888363e98073d0155b364fda5354fd98` |
| `en_us_long_chunked_04` | `en_us_long_chunked_04.wav` | `clean_english` | `en` | 15.77 | openslr/librispeech_asr (test-clean, >=14s LiveDecode::Chunked) (`row=137, id=5639-40744-0000, speaker=5639`) | US / General English (long multi-clause >=14s) | `CC-BY-4.0` | `8069fd8ee2a120f435d682a8e27a9435fa6b80122b5d8843779178d19a3c1c2a` |
| `en_in_south_asian_01` | `en_in_south_asian_01.wav` | `accented_english` | `en` | 4.50 | MushanW/GLOBE (Common Voice CC0-1.0) (`row=207, speaker=S_021316`) | South Asian / Indian English | `CC0-1.0` | `29e6099b26fa319542299ad976faf932150fe76aa81f848f9bbf05a3c90d45da` |
| `en_scot_uk_02` | `en_scot_uk_02.wav` | `accented_english` | `en` | 3.80 | MushanW/GLOBE (Common Voice CC0-1.0) (`row=228, speaker=S_000423`) | Scottish (UK) English | `CC0-1.0` | `9edb7ecea910a451a97a9a663a0fc0417baa5480d4c556cc76884f6210fd5128` |
| `en_ie_irish_03` | `en_ie_irish_03.wav` | `accented_english` | `en` | 3.50 | MushanW/GLOBE (Common Voice CC0-1.0) (`row=355, speaker=S_002441`) | Irish English | `CC0-1.0` | `3d39ef9d461aa38985f0496cf1929233a185afae6cf1be385837f7ea617893c8` |
| `en_l2_german_04` | `en_l2_german_04.wav` | `accented_english` | `en` | 4.40 | MushanW/GLOBE (Common Voice CC0-1.0) (`row=14, speaker=S_001001`) | Non-native L2 (German L1) English | `CC0-1.0` | `ac48d2397d1b8970fef947413fbf1638d5451ad1f20f4527da2dbeb55b9deba6` |
| `en_in_long_chunked_05` | `en_in_long_chunked_05.wav` | `accented_english` | `en` | 17.86 | MushanW/GLOBE (Common Voice CC0-1.0, >=14s LiveDecode::Chunked) (`row=2000(spk=S_001110), row=2007(spk=S_001110), row=2009(spk=S_001110), row=2011(spk=S_001110)`) | South Asian / Indian English (multi-sentence >=14s) | `CC0-1.0` | `3e094516a3b54c0d04f6a38d3677ce35ead906624b0a5e0ba5c6eb82a81e37d6` |
| `hi_in_fleurs_01` | `hi_in_fleurs_01.wav` | `multilingual` | `hi` | 9.12 | google/fleurs (hi_in/test) (`fleurs_id=1766, file=10011266027513218401.wav`) | Hindi / India (hi_in, Devanagari, MALE) | `CC-BY-4.0` | `0dcbc334fb33b946a8a6058314d389d596c3385585b4ffbfc43625b0db9e73d2` |
| `hi_in_fleurs_02` | `hi_in_fleurs_02.wav` | `multilingual` | `hi` | 10.62 | google/fleurs (hi_in/test) (`fleurs_id=1666, file=10107687492910459268.wav`) | Hindi / India (hi_in, Devanagari, FEMALE) | `CC-BY-4.0` | `5ddbe4c369994369912d5b8acd4a4d74a3d2e3d179aa18ca49491b173ba73b90` |
| `ur_pk_fleurs_01` | `ur_pk_fleurs_01.wav` | `multilingual` | `ur` | 6.24 | google/fleurs (ur_pk/test) (`fleurs_id=1930, file=10095499047464363355.wav`) | Urdu / Pakistan-India (ur_pk, Perso-Arabic, FEMALE) | `CC-BY-4.0` | `4166fb85b7a5844eb6d5097ef2a055aa16ee2b98efae8734219884086c08ef2d` |
| `ur_pk_fleurs_02` | `ur_pk_fleurs_02.wav` | `multilingual` | `ur` | 4.92 | google/fleurs (ur_pk/test) (`fleurs_id=1791, file=11716123340002594593.wav`) | Urdu / Pakistan-India (ur_pk, Perso-Arabic, FEMALE) | `CC-BY-4.0` | `060c9001e8354edf57ee2d6de0bd11c1b4fb33a678ae3f47bdeed5ce0995333f` |
| `es_419_fleurs_01` | `es_419_fleurs_01.wav` | `multilingual` | `es` | 9.72 | google/fleurs (es_419/test) (`fleurs_id=1764, file=1008205615316519958.wav`) | Spanish / Latin America (es_419, MALE) | `CC-BY-4.0` | `d49a4b12fb7d7708a1f9a82a8eff4dd939caa576c51f45e9b9cadfa5f385b7e8` |
| `de_de_fleurs_01` | `de_de_fleurs_01.wav` | `multilingual` | `de` | 10.68 | google/fleurs (de_de/test) (`fleurs_id=1683, file=10030056357041867775.wav`) | German / Germany (de_de, MALE) | `CC-BY-4.0` | `1173d376945fd7a6ab43540b93b4560b4493b6ec32e015d31fe41b447b26c8fd` |
| `de_de_fleurs_02` | `de_de_fleurs_02.wav` | `multilingual` | `de` | 9.90 | google/fleurs (de_de/test) (`fleurs_id=1739, file=10058186628567796965.wav`) | German / Germany (de_de, MALE) | `CC-BY-4.0` | `eb032aee601f365430df5626d1f63bad49422e67edee5b347c67679297832596` |
| `silence_room_tone_01` | `silence_room_tone_01.wav` | `silence_breath` | `—` | 2.50 | Deterministic lowyap-eval pure room-tone generator (seed 0xA11CE0001) (`seed=0xA11CE0001, -50dBFS pink room tone, 2.50s`) | Ambient room tone only (0 speech) | `MIT / CC0-1.0` | `213364c5b6fc01cfbb99827a7f3d2a850dba57bbff5f2995607afc72ae3521bf` |
| `silence_breath_puff_02` | `silence_breath_puff_02.wav` | `silence_breath` | `—` | 2.50 | Deterministic lowyap-eval room-tone + breath-puff generator (seed 0xB2EA74002) (`seed=0xB2EA74002, room tone + 420ms soft breath puff, 2.50s`) | Ambient room tone + soft breath exhalation (0 speech) | `MIT / CC0-1.0` | `7ec1caa0de48b738968f1cc57ad891e7eb89e50235e7a7d58f9186e86f053afb` |

## Reference Transcripts (Speech Clips)

- **`en_us_clean_01`** (`en`): Congratulations were poured in upon the princess everywhere during her journey
- **`en_us_clean_02`** (`en`): After proceeding a few miles the progress of hawkeye who led the advance became more deliberate and watchful
- **`en_us_fleurs_03`** (`en`): All nouns, alongside the word Sie for you, always begin with a capital letter, even in the middle of a sentence.
- **`en_us_long_chunked_04`** (`en`): Eleven o'clock had struck it was a fine clear night they were the only persons on the road and they sauntered leisurely along to avoid paying the price of fatigue for the recreation provided for the toledans in their valley or on the banks of their river
- **`en_in_south_asian_01`** (`en`): they are distributed by walt disney pictures unless otherwise noted.
- **`en_scot_uk_02`** (`en`): numerous stories are associated with the time preceding her death.
- **`en_ie_irish_03`** (`en`): the town of java is on the western border of wyoming county.
- **`en_l2_german_04`** (`en`): it occurs further north in northern mexico, and into western texas.
- **`en_in_long_chunked_05`** (`en`): love is a fluid concept, and i'm not sure if androids get that. i would give a good review of the new tv i bought a week ago. at the airport, the woman threw herself into revision for college so she could experience university. after a tragic accident with a chainsaw he has become a paraplegic.
- **`hi_in_fleurs_01`** (`hi`): कुछ अणुओं में अस्थिर केंद्रक होता है, जिसका मतलब यह है कि उनमें थोड़े या बिना किसी झटके से टूटने की प्रवृत्ति होती है,
- **`hi_in_fleurs_02`** (`hi`): अपनी सरकार के अलावा आप अन्य देशों की सरकारों की सलाह ले सकते हैं. हालाँकि, उनकी सलाह उनके नागरिकों को ध्यान में रखकर दी जाती हैं.
- **`ur_pk_fleurs_01`** (`ur`): فلپائنی فوٹوگرافروں کے ساتھ چھےاغوا شدگان کو جلد رہا کر دیا گیا تھا، جِن میں بچّے اور بُوڑھے شامل تھے۔
- **`ur_pk_fleurs_02`** (`ur`): اس سائٹ کے ساختوں پر نشان لگا کر یا گرافیٹی کھرچ کر اسے مسخ نہ کریں۔
- **`es_419_fleurs_01`** (`es`): Fue tanta la cantidad de gente que se concentró, que no todos pudieron acceder al funeral en la Plaza de San Pedro.
- **`de_de_fleurs_01`** (`de`): Es leben noch viele Männer und Frauen, die ihre Zeit hier überlebt haben, und viele weitere mit Angehörigen, die dort ermordet wurden oder sich zu Tode arbeiteten, Juden und Nichtjuden gleichermaßen.
- **`de_de_fleurs_02`** (`de`): Die Höhle befindet sich auf der Spitze eines der Berge nördlich von Mekka und ist vom Rest der Welt völlig isoliert.
