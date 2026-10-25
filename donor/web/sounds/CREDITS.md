# Звуки: источники и лицензии

Набор по умолчанию — только свободные записи: CC0 (общественное достояние) и CC BY (с указанием автора). Файлы Mojang / Minecraft сюда не входят: их игрок может взять из своей игры (настройки → «Звук» → «Звуки Minecraft»), они остаются только в его браузере. Все файлы обработаны скриптом `tools/sounds_build.py` (нарезка, моно, срез тишины, плавные края, выравнивание громкости по EBU R128, Ogg Vorbis); синтез в коде (`web/audio/synth.js`) — только запасной. Записи Freesound взяты как публичные превью (так помечено в источниках).

## Требуют указания автора (CC BY)

- Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) — https://freesound.org/people/Eelke/sounds/462598/ — CC BY 3.0 (https://creativecommons.org/licenses/by/3.0/)
- Lamoot — https://opengameart.org/content/sheep-sound-bleats-yo-frankie — CC BY 3.0 (https://creativecommons.org/licenses/by/3.0/)

Остальные — CC0 1.0 (https://creativecommons.org/publicdomain/zero/1.0/).

## Звуки и громкость

Громкость файла — самый громкий миг (EBU R128 momentary), цель −16 LUFS (острые звуки упираются в пик −1 дБFS и бывают тише); в игре на неё умножаются множители события как в MC (шаг 0.15, удар 0.25, разрушение 1.0 — web/audio/events.js, catalog.js) и ±5 % высоты.

| звук | файлов | громкость файлов, LUFS | длина, с |
|---|---|---|---|
| `block.stone.dig` | 6 | -20.9…-16.0 | 0.40…0.59 |
| `block.stone.step` | 6 | -16.0…-16.0 | 0.31…0.36 |
| `block.wood.dig` | 6 | -16.7…-16.0 | 0.17…0.20 |
| `block.wood.step` | 6 | -19.2…-17.8 | 0.25…0.32 |
| `block.gravel.dig` | 6 | -23.6…-16.0 | 0.33…0.70 |
| `block.gravel.step` | 6 | -18.0…-16.0 | 0.11…0.25 |
| `block.grass.dig` | 6 | -16.0…-16.0 | 0.38…0.80 |
| `block.grass.step` | 6 | -18.6…-16.0 | 0.10…0.38 |
| `block.sand.dig` | 6 | -19.7…-16.0 | 0.28…0.80 |
| `block.sand.step` | 6 | -19.2…-16.0 | 0.31…0.43 |
| `block.snow.dig` | 6 | -17.6…-16.0 | 0.20…0.80 |
| `block.snow.step` | 6 | -18.6…-16.0 | 0.19…0.20 |
| `block.cloth.dig` | 6 | -17.9…-16.0 | 0.36…0.68 |
| `block.cloth.step` | 5 | -23.2…-17.4 | 0.09…0.15 |
| `block.glass.break` | 6 | -18.3…-16.0 | 0.34…0.75 |
| `block.ladder.step` | 6 | -18.2…-16.0 | 0.14…0.35 |
| `item.pickup` | 1 | -20.1…-20.1 | 0.18…0.18 |
| `chest.open` | 3 | -16.0…-16.0 | 0.30…0.75 |
| `chest.close` | 4 | -16.0…-16.0 | 0.44…0.70 |
| `furnace.crackle` | 5 | -18.6…-16.0 | 0.50…0.50 |
| `ui.click` | 1 | -21.6…-21.6 | 0.06…0.06 |
| `player.hurt` | 6 | -16.0…-16.0 | 0.25…0.40 |
| `player.hurt_drown` | 4 | -18.1…-16.0 | 0.26…0.54 |
| `player.death` | 3 | -16.0…-16.0 | 0.34…0.80 |
| `player.small_fall` | 1 | -16.0…-16.0 | 0.53…0.53 |
| `player.big_fall` | 1 | -16.0…-16.0 | 0.57…0.57 |
| `player.eat` | 5 | -19.7…-16.0 | 0.33…0.50 |
| `player.eat_done` | 3 | -18.6…-16.0 | 0.60…0.60 |
| `player.burp` | 2 | -16.0…-16.0 | 0.53…0.74 |
| `player.splash` | 6 | -17.7…-16.0 | 0.38…0.60 |
| `player.splash_high` | 3 | -20.5…-16.0 | 0.74…1.37 |
| `player.swim` | 5 | -16.0…-16.0 | 0.26…0.70 |
| `player.attack_strong` | 3 | -16.0…-16.0 | 0.32…0.46 |
| `player.attack_weak` | 3 | -16.0…-16.0 | 0.25…0.29 |
| `player.attack_crit` | 3 | -16.0…-16.0 | 0.20…0.38 |
| `player.attack_knockback` | 2 | -16.0…-16.0 | 0.33…0.38 |
| `item.break` | 3 | -19.0…-16.0 | 0.12…0.30 |
| `item.armor_equip` | 5 | -16.0…-16.0 | 0.14…0.38 |
| `bucket.fill` | 4 | -16.0…-16.0 | 0.54…0.80 |
| `bucket.empty` | 3 | -16.0…-16.0 | 0.99…1.00 |
| `ambient.underwater_enter` | 3 | -19.5…-16.0 | 0.42…1.35 |
| `ambient.underwater_exit` | 3 | -20.8…-16.2 | 0.18…0.23 |
| `ambient.underwater_loop` | 1 | -16.0…-16.0 | 6.00…6.00 |
| `item.splash` | 4 | -20.8…-16.0 | 0.18…0.40 |
| `machine.start` | 1 | -16.4…-16.4 | 0.17…0.17 |
| `machine.stop` | 1 | -18.5…-18.5 | 0.36…0.36 |
| `machine.zap` | 3 | -19.9…-16.0 | 0.22…0.50 |
| `machine.loop` | 1 | -16.0…-16.0 | 2.18…2.18 |
| `machine.generator_loop` | 1 | -16.0…-16.0 | 5.14…5.14 |
| `entity.pig.ambient` | 5 | -16.0…-16.0 | 0.29…0.93 |
| `entity.pig.hurt` | 3 | -16.0…-16.0 | 0.81…0.92 |
| `entity.pig.death` | 2 | -16.0…-16.0 | 0.87…1.54 |
| `entity.pig.step` | 5 | -17.0…-16.1 | 0.12…0.18 |
| `entity.cow.ambient` | 5 | -16.0…-16.0 | 1.56…1.80 |
| `entity.cow.hurt` | 3 | -16.0…-16.0 | 0.59…0.83 |
| `entity.cow.step` | 4 | -16.0…-16.0 | 0.50…0.57 |
| `entity.sheep.ambient` | 4 | -16.0…-16.0 | 0.78…1.26 |
| `entity.sheep.step` | 5 | -17.0…-16.1 | 0.12…0.18 |
| `entity.sheep.shear` | 2 | -16.0…-16.0 | 0.34…0.41 |
| `entity.chicken.ambient` | 5 | -16.0…-16.0 | 0.24…0.70 |
| `entity.chicken.hurt` | 2 | -16.0…-16.0 | 0.59…0.60 |
| `entity.chicken.step` | 2 | -19.4…-17.6 | 0.12…0.16 |
| `entity.chicken.egg` | 2 | -23.9…-18.2 | 0.07…0.18 |
| `entity.zombie.ambient` | 6 | -16.0…-16.0 | 1.50…1.70 |
| `entity.zombie.hurt` | 5 | -17.1…-16.0 | 0.21…0.43 |
| `entity.zombie.death` | 4 | -16.0…-16.0 | 0.48…0.65 |
| `entity.zombie.step` | 5 | -19.1…-17.1 | 0.20…0.26 |
| `entity.generic.extinguish_fire` | 2 | -16.0…-16.0 | 0.98…1.20 |

## Все файлы

| файл в игре | исходник | автор | лицензия | ссылка |
|---|---|---|---|---|
| `block/stone/dig_1.ogg` | `impactMining_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/stone/dig_2.ogg` | `impactMining_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/stone/dig_3.ogg` | `impactMining_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/stone/dig_4.ogg` | `impactMining_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/stone/dig_5.ogg` | `impactMining_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/stone/dig_6.ogg` | `bfh1_rock_breaking_03.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/stone/step_1.ogg` | `Fantozzi-StoneL1.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/stone/step_2.ogg` | `Fantozzi-StoneL2.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/stone/step_3.ogg` | `Fantozzi-StoneL3.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/stone/step_4.ogg` | `Fantozzi-StoneR1.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/stone/step_5.ogg` | `Fantozzi-StoneR2.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/stone/step_6.ogg` | `Fantozzi-StoneR3.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/wood/dig_1.ogg` | `impactWood_medium_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/dig_2.ogg` | `impactWood_medium_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/dig_3.ogg` | `impactWood_medium_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/dig_4.ogg` | `impactWood_medium_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/dig_5.ogg` | `impactWood_medium_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/dig_6.ogg` | `impactPlank_medium_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/step_1.ogg` | `footstep_wood_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/step_2.ogg` | `footstep_wood_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/step_3.ogg` | `footstep_wood_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/step_4.ogg` | `footstep_wood_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/step_5.ogg` | `footstep_wood_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/wood/step_6.ogg` | `wood03.ogg` | TinyWorlds (записи pdsounds.org) | CC0 1.0 | https://opengameart.org/content/different-steps-on-wood-stone-leaves-gravel-and-mud |
| `block/gravel/dig_1.ogg` | `fs179341_Gravel_Impacts_and_Falls.mp3 0.42–1 с` | lolamadeus | CC0 1.0 | https://freesound.org/people/lolamadeus/sounds/179341/ |
| `block/gravel/dig_2.ogg` | `fs179341_Gravel_Impacts_and_Falls.mp3 2.3–2.9 с` | lolamadeus | CC0 1.0 | https://freesound.org/people/lolamadeus/sounds/179341/ |
| `block/gravel/dig_3.ogg` | `fs179341_Gravel_Impacts_and_Falls.mp3 2.98–3.6 с` | lolamadeus | CC0 1.0 | https://freesound.org/people/lolamadeus/sounds/179341/ |
| `block/gravel/dig_4.ogg` | `fs787858_Gravels_lifting_with_a_Shovel.mp3 0.33–0.95 с` | Zeinel-2-and-Hope-sounds-3 | CC0 1.0 | https://freesound.org/people/Zeinel-2-and-Hope-sounds-3/sounds/787858/ |
| `block/gravel/dig_5.ogg` | `fs787858_Gravels_lifting_with_a_Shovel.mp3 1.7–2.4 с` | Zeinel-2-and-Hope-sounds-3 | CC0 1.0 | https://freesound.org/people/Zeinel-2-and-Hope-sounds-3/sounds/787858/ |
| `block/gravel/dig_6.ogg` | `fs787858_Gravels_lifting_with_a_Shovel.mp3 6.17–6.51 с` | Zeinel-2-and-Hope-sounds-3 | CC0 1.0 | https://freesound.org/people/Zeinel-2-and-Hope-sounds-3/sounds/787858/ |
| `block/gravel/step_1.ogg` | `0.ogg` | Ali_6868 (Freesound, набор «Gravel Footsteps»; нарезка congusbongus) | CC0 1.0 | https://opengameart.org/content/footsteps-on-different-surfaces |
| `block/gravel/step_2.ogg` | `1.ogg` | Ali_6868 (Freesound, набор «Gravel Footsteps»; нарезка congusbongus) | CC0 1.0 | https://opengameart.org/content/footsteps-on-different-surfaces |
| `block/gravel/step_3.ogg` | `2.ogg` | Ali_6868 (Freesound, набор «Gravel Footsteps»; нарезка congusbongus) | CC0 1.0 | https://opengameart.org/content/footsteps-on-different-surfaces |
| `block/gravel/step_4.ogg` | `3.ogg` | Ali_6868 (Freesound, набор «Gravel Footsteps»; нарезка congusbongus) | CC0 1.0 | https://opengameart.org/content/footsteps-on-different-surfaces |
| `block/gravel/step_5.ogg` | `4.ogg` | Ali_6868 (Freesound, набор «Gravel Footsteps»; нарезка congusbongus) | CC0 1.0 | https://opengameart.org/content/footsteps-on-different-surfaces |
| `block/gravel/step_6.ogg` | `5.ogg` | Ali_6868 (Freesound, набор «Gravel Footsteps»; нарезка congusbongus) | CC0 1.0 | https://opengameart.org/content/footsteps-on-different-surfaces |
| `block/grass/dig_1.ogg` | `fs787925_Digging_dirt_and_grass_loop.mp3 0–0.68 с` | modusmogulus | CC0 1.0 | https://freesound.org/people/modusmogulus/sounds/787925/ |
| `block/grass/dig_2.ogg` | `fs787925_Digging_dirt_and_grass_loop.mp3 1–1.78 с` | modusmogulus | CC0 1.0 | https://freesound.org/people/modusmogulus/sounds/787925/ |
| `block/grass/dig_3.ogg` | `fs787925_Digging_dirt_and_grass_loop.mp3 2.14–3 с` | modusmogulus | CC0 1.0 | https://freesound.org/people/modusmogulus/sounds/787925/ |
| `block/grass/dig_4.ogg` | `fs787925_Digging_dirt_and_grass_loop.mp3 3.2–3.96 с` | modusmogulus | CC0 1.0 | https://freesound.org/people/modusmogulus/sounds/787925/ |
| `block/grass/dig_5.ogg` | `leaves01.ogg` | TinyWorlds (записи pdsounds.org) | CC0 1.0 | https://opengameart.org/content/different-steps-on-wood-stone-leaves-gravel-and-mud |
| `block/grass/dig_6.ogg` | `fs540264_dig.mp3 0–0.47 с` | zepurple | CC0 1.0 | https://freesound.org/people/zepurple/sounds/540264/ |
| `block/grass/step_1.ogg` | `footstep_grass_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/grass/step_2.ogg` | `footstep_grass_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/grass/step_3.ogg` | `footstep_grass_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/grass/step_4.ogg` | `footstep_grass_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/grass/step_5.ogg` | `footstep_grass_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/grass/step_6.ogg` | `leaves01.ogg` | TinyWorlds (записи pdsounds.org) | CC0 1.0 | https://opengameart.org/content/different-steps-on-wood-stone-leaves-gravel-and-mud |
| `block/sand/dig_1.ogg` | `fs349457_Dig_wet_sand-02.mp3` | RUncELL | CC0 1.0 | https://freesound.org/people/RUncELL/sounds/349457/ |
| `block/sand/dig_2.ogg` | `fs349459_Dig_wet_sand-04.mp3` | RUncELL | CC0 1.0 | https://freesound.org/people/RUncELL/sounds/349459/ |
| `block/sand/dig_3.ogg` | `fs349456_Dig_wet_sand-01.mp3 0–0.54 с` | RUncELL | CC0 1.0 | https://freesound.org/people/RUncELL/sounds/349456/ |
| `block/sand/dig_4.ogg` | `fs349460_Dig_wet_sand-05.mp3 0–0.8 с` | RUncELL | CC0 1.0 | https://freesound.org/people/RUncELL/sounds/349460/ |
| `block/sand/dig_5.ogg` | `fs651293_Digging_in_wet_course_sand_2_.mp3 0.33–0.63 с` | f3bbbo | CC0 1.0 | https://freesound.org/people/f3bbbo/sounds/651293/ |
| `block/sand/dig_6.ogg` | `fs651292_Digging_in_wet_course_sand_1_.mp3` | f3bbbo | CC0 1.0 | https://freesound.org/people/f3bbbo/sounds/651292/ |
| `block/sand/step_1.ogg` | `Fantozzi-SandL1.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/sand/step_2.ogg` | `Fantozzi-SandR1.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/sand/step_3.ogg` | `Fantozzi-SandL2.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/sand/step_4.ogg` | `Fantozzi-SandR2.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/sand/step_5.ogg` | `Fantozzi-SandL3.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/sand/step_6.ogg` | `Fantozzi-SandR3.ogg` | Fantozzi (через qubodup) | CC0 1.0 | https://opengameart.org/content/fantozzis-footsteps-grasssand-stone |
| `block/snow/dig_1.ogg` | `fs450848_snow_block_of_snow_igloo_brick_put_down_.mp3` | kyles | CC0 1.0 | https://freesound.org/people/kyles/sounds/450848/ |
| `block/snow/dig_2.ogg` | `fs384424_Footstep_in_the_snow_04_RAW_.mp3` | cabled_mess | CC0 1.0 | https://freesound.org/people/cabled_mess/sounds/384424/ |
| `block/snow/dig_3.ogg` | `fs234784_Crunch_Bone_break_walking_on_snow.mp3 0.09–0.32 с` | wubitog | CC0 1.0 | https://freesound.org/people/wubitog/sounds/234784/ |
| `block/snow/dig_4.ogg` | `fs234784_Crunch_Bone_break_walking_on_snow.mp3 0.61–0.81 с` | wubitog | CC0 1.0 | https://freesound.org/people/wubitog/sounds/234784/ |
| `block/snow/dig_5.ogg` | `fs681611_Snow_Shovel_Driveway.mp3 0.77–1.3 с` | bodallasfan888 | CC0 1.0 | https://freesound.org/people/bodallasfan888/sounds/681611/ |
| `block/snow/dig_6.ogg` | `fs681611_Snow_Shovel_Driveway.mp3 2.24–2.8 с` | bodallasfan888 | CC0 1.0 | https://freesound.org/people/bodallasfan888/sounds/681611/ |
| `block/snow/step_1.ogg` | `footstep_snow_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/snow/step_2.ogg` | `footstep_snow_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/snow/step_3.ogg` | `footstep_snow_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/snow/step_4.ogg` | `footstep_snow_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/snow/step_5.ogg` | `footstep_snow_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/snow/step_6.ogg` | `fs234784_Crunch_Bone_break_walking_on_snow.mp3 0.61–0.81 с` | wubitog | CC0 1.0 | https://freesound.org/people/wubitog/sounds/234784/ |
| `block/cloth/dig_1.ogg` | `cloth1.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `block/cloth/dig_2.ogg` | `cloth2.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `block/cloth/dig_3.ogg` | `cloth3.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `block/cloth/dig_4.ogg` | `cloth4.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `block/cloth/dig_5.ogg` | `clothBelt.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `block/cloth/dig_6.ogg` | `clothBelt2.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `block/cloth/step_1.ogg` | `footstep_carpet_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/cloth/step_2.ogg` | `footstep_carpet_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/cloth/step_3.ogg` | `footstep_carpet_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/cloth/step_4.ogg` | `footstep_carpet_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/cloth/step_5.ogg` | `footstep_carpet_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `block/glass/break_1.ogg` | `bfh1_glass_breaking_01.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/glass/break_2.ogg` | `bfh1_glass_breaking_02.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/glass/break_3.ogg` | `bfh1_glass_breaking_03.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/glass/break_4.ogg` | `bfh1_glass_breaking_04.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/glass/break_5.ogg` | `bfh1_glass_breaking_05.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/glass/break_6.ogg` | `bfh1_glass_breaking_06.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `block/ladder/step_1.ogg` | `0.ogg` | Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) | CC BY 3.0 | https://freesound.org/people/Eelke/sounds/462598/ |
| `block/ladder/step_2.ogg` | `1.ogg` | Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) | CC BY 3.0 | https://freesound.org/people/Eelke/sounds/462598/ |
| `block/ladder/step_3.ogg` | `2.ogg` | Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) | CC BY 3.0 | https://freesound.org/people/Eelke/sounds/462598/ |
| `block/ladder/step_4.ogg` | `3.ogg` | Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) | CC BY 3.0 | https://freesound.org/people/Eelke/sounds/462598/ |
| `block/ladder/step_5.ogg` | `4.ogg` | Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) | CC BY 3.0 | https://freesound.org/people/Eelke/sounds/462598/ |
| `block/ladder/step_6.ogg` | `5.ogg` | Eelke (Freesound 462598 «boots on aluminum ladder 01»; нарезка congusbongus) | CC BY 3.0 | https://freesound.org/people/Eelke/sounds/462598/ |
| `item/pickup_1.ogg` | `rollover1.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/ui-audio |
| `chest/open_1.ogg` | `creak1.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `chest/open_2.ogg` | `creak2.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `chest/open_3.ogg` | `creak3.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `chest/close_1.ogg` | `doorClose_1.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `chest/close_2.ogg` | `doorClose_2.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `chest/close_3.ogg` | `doorClose_3.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `chest/close_4.ogg` | `doorClose_4.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `furnace/crackle_1.ogg` | `fire-1.wav 0–0.5 с` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/fire-crackling |
| `furnace/crackle_2.ogg` | `fire-1.wav 0.55–1.05 с` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/fire-crackling |
| `furnace/crackle_3.ogg` | `fire-1.wav 1.1–1.6 с` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/fire-crackling |
| `furnace/crackle_4.ogg` | `fire-1.wav 1.65–2.15 с` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/fire-crackling |
| `furnace/crackle_5.ogg` | `fire-1.wav 2.2–2.7 с` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/fire-crackling |
| `ui/click_1.ogg` | `mouseclick1.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/ui-audio |
| `player/hurt_1.ogg` | `slightscream-01.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/15-vocal-male-strainhurtpainjump-sounds |
| `player/hurt_2.ogg` | `slightscream-02.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/15-vocal-male-strainhurtpainjump-sounds |
| `player/hurt_3.ogg` | `slightscream-08.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/15-vocal-male-strainhurtpainjump-sounds |
| `player/hurt_4.ogg` | `slightscream-11.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/15-vocal-male-strainhurtpainjump-sounds |
| `player/hurt_5.ogg` | `slightscream-12.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/15-vocal-male-strainhurtpainjump-sounds |
| `player/hurt_6.ogg` | `fs47356_oof.mp3` | fotoshop | CC0 1.0 | https://freesound.org/people/fotoshop/sounds/47356/ |
| `player/hurt_drown_1.ogg` | `bubble_01.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/hurt_drown_2.ogg` | `bubble_02.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/hurt_drown_3.ogg` | `bubble_03.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/hurt_drown_4.ogg` | `bubbles-single2.wav` | BMacZero | CC0 1.0 | https://opengameart.org/content/bubble-sound-effects |
| `player/death_1.ogg` | `fs416838_Grunt2_-_Death_Pain.mp3 0.15–0.9 с` | tonsil5 | CC0 1.0 | https://freesound.org/people/tonsil5/sounds/416838/ |
| `player/death_2.ogg` | `fs416839_Grunt1_-_Death_Pain.mp3 0.09–0.6 с` | tonsil5 | CC0 1.0 | https://freesound.org/people/tonsil5/sounds/416839/ |
| `player/death_3.ogg` | `fs397276_Grunt1.mp3` | whisperbandnumber1 | CC0 1.0 | https://freesound.org/people/whisperbandnumber1/sounds/397276/ |
| `player/small_fall_1.ogg` | `impactSoft_heavy_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/big_fall_1.ogg` | `impactSoft_heavy_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/eat_1.ogg` | `crunch.2.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/eat_2.ogg` | `crunch.3.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/eat_3.ogg` | `crunch.4.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/eat_4.ogg` | `crunch.5.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/eat_5.ogg` | `crunch.6.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/eat_done_1.ogg` | `apple_bite_0.ogg` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/apple-bite |
| `player/eat_done_2.ogg` | `crunch.1.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/eat_done_3.ogg` | `crunch.7.ogg` | StarNinjas | CC0 1.0 | https://opengameart.org/content/7-eating-crunches |
| `player/burp_1.ogg` | `fs570266_The_Best_Burp_Sample_On_The_Internet.mp3` | scooterfort | CC0 1.0 | https://freesound.org/people/scooterfort/sounds/570266/ |
| `player/burp_2.ogg` | `fs26762_belch1.mp3` | santuccie | CC0 1.0 | https://freesound.org/people/santuccie/sounds/26762/ |
| `player/splash_1.ogg` | `splash_02.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_2.ogg` | `splash_06.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_3.ogg` | `splash_08.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_4.ogg` | `splash_13.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_5.ogg` | `splash_14.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_6.ogg` | `splash_15.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_high_1.ogg` | `cannon_miss_0.ogg` | Thimras | CC0 1.0 | https://opengameart.org/content/ocean-splash |
| `player/splash_high_2.ogg` | `splash_04.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/splash_high_3.ogg` | `splash_01.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `player/swim_1.ogg` | `fs267566_Swim_Sound.mp3 0.37–0.76 с` | Snudio_Records | CC0 1.0 | https://freesound.org/people/Snudio_Records/sounds/267566/ |
| `player/swim_2.ogg` | `fs267566_Swim_Sound.mp3 1.52–2.16 с` | Snudio_Records | CC0 1.0 | https://freesound.org/people/Snudio_Records/sounds/267566/ |
| `player/swim_3.ogg` | `fs267566_Swim_Sound.mp3 2.67–3.38 с` | Snudio_Records | CC0 1.0 | https://freesound.org/people/Snudio_Records/sounds/267566/ |
| `player/swim_4.ogg` | `fs267566_Swim_Sound.mp3 4.05–4.72 с` | Snudio_Records | CC0 1.0 | https://freesound.org/people/Snudio_Records/sounds/267566/ |
| `player/swim_5.ogg` | `fs391666_swim.mp3` | jeckkech | CC0 1.0 | https://freesound.org/people/jeckkech/sounds/391666/ |
| `player/attack_strong_1.ogg` | `impactPunch_heavy_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_strong_2.ogg` | `impactPunch_heavy_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_strong_3.ogg` | `impactPunch_heavy_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_weak_1.ogg` | `impactPunch_medium_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_weak_2.ogg` | `impactPunch_medium_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_weak_3.ogg` | `impactPunch_medium_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_crit_1.ogg` | `impactPunch_heavy_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_crit_2.ogg` | `blade_01.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/80-cc0-rpg-sfx |
| `player/attack_crit_3.ogg` | `blade_02.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/80-cc0-rpg-sfx |
| `player/attack_knockback_1.ogg` | `impactPunch_heavy_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `player/attack_knockback_2.ogg` | `impactPunch_heavy_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `item/break_1.ogg` | `fs215382_pencilsnap.mp3` | Reitanna | CC0 1.0 | https://freesound.org/people/Reitanna/sounds/215382/ |
| `item/break_2.ogg` | `bfh1_wood_breaking_03.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/75-cc0-breaking-falling-hit-sfx |
| `item/break_3.ogg` | `fs251464_twig_snap_classic.mp3` | martian | CC0 1.0 | https://freesound.org/people/martian/sounds/251464/ |
| `item/armor_equip_1.ogg` | `impactMetal_light_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `item/armor_equip_2.ogg` | `impactMetal_light_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `item/armor_equip_3.ogg` | `impactMetal_light_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `item/armor_equip_4.ogg` | `chain_01.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/80-cc0-rpg-sfx |
| `item/armor_equip_5.ogg` | `chain_02.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/80-cc0-rpg-sfx |
| `bucket/fill_1.ogg` | `fs613483_ScoopingWater.mp3 0.14–0.68 с` | Arcorias | CC0 1.0 | https://freesound.org/people/Arcorias/sounds/613483/ |
| `bucket/fill_2.ogg` | `fs613483_ScoopingWater.mp3 2.08–2.84 с` | Arcorias | CC0 1.0 | https://freesound.org/people/Arcorias/sounds/613483/ |
| `bucket/fill_3.ogg` | `fs613483_ScoopingWater.mp3 4.08–4.67 с` | Arcorias | CC0 1.0 | https://freesound.org/people/Arcorias/sounds/613483/ |
| `bucket/fill_4.ogg` | `fs540006_Elm_Cir_7_1MOISES_M_FILLING_BUCKET_1.mp3 0–0.8 с` | moma18 | CC0 1.0 | https://freesound.org/people/moma18/sounds/540006/ |
| `bucket/empty_1.ogg` | `fs378317_pouring_bucket.mp3 0.11–1.1 с` | 13F_Panska_Koprivikova_Klara | CC0 1.0 | https://freesound.org/people/13F_Panska_Koprivikova_Klara/sounds/378317/ |
| `bucket/empty_2.ogg` | `fs427392_Pouring_Water_Short_.mp3 0–1 с` | zembacraftworks | CC0 1.0 | https://freesound.org/people/zembacraftworks/sounds/427392/ |
| `bucket/empty_3.ogg` | `fs271369_Water_Into_Pail.mp3 0.1–1.1 с` | bashworth | CC0 1.0 | https://freesound.org/people/bashworth/sounds/271369/ |
| `ambient/underwater_enter_1.ogg` | `fs679404_Dipping_below_water.mp3 1.33–2.68 с` | adviseme333 | CC0 1.0 | https://freesound.org/people/adviseme333/sounds/679404/ |
| `ambient/underwater_enter_2.ogg` | `fs679407_Dipping_below_water_2.mp3 2.2–3.55 с` | adviseme333 | CC0 1.0 | https://freesound.org/people/adviseme333/sounds/679407/ |
| `ambient/underwater_enter_3.ogg` | `bubbles-single2.wav` | BMacZero | CC0 1.0 | https://opengameart.org/content/bubble-sound-effects |
| `ambient/underwater_exit_1.ogg` | `splash_09.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `ambient/underwater_exit_2.ogg` | `splash_10.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `ambient/underwater_exit_3.ogg` | `water_splash-05.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/6-short-water-splashes |
| `ambient/underwater_loop_1.ogg` | `underwater_or_space_engine_0.ogg 10–16.25 с` | gmason | CC0 1.0 | https://opengameart.org/content/underwater-or-space-engine-rumble |
| `item/splash_1.ogg` | `splash_09.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `item/splash_2.ogg` | `splash_10.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/40-cc0-water-splash-slime-sfx |
| `item/splash_3.ogg` | `water_splash-02.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/6-short-water-splashes |
| `item/splash_4.ogg` | `water_splash-05.flac` | qubodup | CC0 1.0 | https://opengameart.org/content/6-short-water-splashes |
| `machine/start_1.ogg` | `metalLatch.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `machine/stop_1.ogg` | `metalClick.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `machine/zap_1.ogg` | `spark.wav` | BMacZero | CC0 1.0 | https://opengameart.org/content/electricity-sound-effects-0 |
| `machine/zap_2.ogg` | `continuousspark.wav` | BMacZero | CC0 1.0 | https://opengameart.org/content/electricity-sound-effects-0 |
| `machine/zap_3.ogg` | `buzz_0.ogg 0–0.5 с` | themightyglider | CC0 1.0 | https://opengameart.org/content/electric-buzz |
| `machine/loop_1.ogg` | `fs700034_Electric_Train_Motor_Idle_-_Loop_Queensl.mp3 0–2.45 с` | JotrainG | CC0 1.0 | https://freesound.org/people/JotrainG/sounds/700034/ |
| `machine/generator_loop_1.ogg` | `loop_generator_1.mp3 0.05–5.45 с` | YCbCr | CC0 1.0 | https://opengameart.org/content/generator-loop |
| `entity/pig/ambient_1.ogg` | `fs233169_Pig_-_Grunt_1_-_96kHz.mp3 0.07–0.74 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233169/ |
| `entity/pig/ambient_2.ogg` | `fs233170_Pig_-_Grunt_2_-_96kHz.mp3 0.09–0.77 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233170/ |
| `entity/pig/ambient_3.ogg` | `fs233171_Pig_-_Grunt_3_-_96kHz.mp3 0.27–1.1 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233171/ |
| `entity/pig/ambient_4.ogg` | `fs233172_Pig_-_Grunt_4_-_96kHz.mp3 0.15–1.13 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233172/ |
| `entity/pig/ambient_5.ogg` | `fs442907_Pig_Grunt.mp3 0–0.29 с` | qubodup | CC0 1.0 | https://freesound.org/people/qubodup/sounds/442907/ |
| `entity/pig/hurt_1.ogg` | `fs233175_Pig_-_Grunt_and_Squeal_2_-_96kHz.mp3 0.18–1.1 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233175/ |
| `entity/pig/hurt_2.ogg` | `fs233176_Pig_-_Grunt_and_Squeal_3_-_96kHz.mp3 0.1–1 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233176/ |
| `entity/pig/hurt_3.ogg` | `fs233177_Pig_-_Grunt_and_Squeal_4_-_96kHz.mp3 0.16–0.97 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233177/ |
| `entity/pig/death_1.ogg` | `fs233178_Pig_-_Multiple_Squeals_1_2_pigs_-_96kHz.mp3 0.45–1.99 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233178/ |
| `entity/pig/death_2.ogg` | `fs233173_Pig_-_Grunt_5_deep_-_96kHz.mp3 0.77–1.65 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233173/ |
| `entity/pig/step_1.ogg` | `impactSoft_medium_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/pig/step_2.ogg` | `impactSoft_medium_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/pig/step_3.ogg` | `impactSoft_medium_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/pig/step_4.ogg` | `impactSoft_medium_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/pig/step_5.ogg` | `impactSoft_medium_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/cow/ambient_1.ogg` | `fs233128_Cow_-_Moo_3_-_96kHz.mp3 0.23–1.83 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233128/ |
| `entity/cow/ambient_2.ogg` | `fs233129_Cow_-_Moo_2_-_96kHz.mp3 0–1.82 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233129/ |
| `entity/cow/ambient_3.ogg` | `fs233134_Cow_-_Moo_5_-_96kHz.mp3 0.58–2.23 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233134/ |
| `entity/cow/ambient_4.ogg` | `fs59245_z-moo01.mp3` | Zozzy | CC0 1.0 | https://freesound.org/people/Zozzy/sounds/59245/ |
| `entity/cow/ambient_5.ogg` | `fs275154_Moo.mp3` | Bird_man | CC0 1.0 | https://freesound.org/people/Bird_man/sounds/275154/ |
| `entity/cow/hurt_1.ogg` | `fs233141_Cow_-_Moan_1_-_96kHz.mp3 0.4–0.99 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233141/ |
| `entity/cow/hurt_2.ogg` | `fs233146_Cow_-_Moan_2_-_96kHz.mp3 0.47–1.3 с` | JarredGibb | CC0 1.0 | https://freesound.org/people/JarredGibb/sounds/233146/ |
| `entity/cow/hurt_3.ogg` | `fs59245_z-moo01.mp3 0–0.8 с` | Zozzy | CC0 1.0 | https://freesound.org/people/Zozzy/sounds/59245/ |
| `entity/cow/step_1.ogg` | `impactSoft_heavy_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/cow/step_2.ogg` | `impactSoft_heavy_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/cow/step_3.ogg` | `impactSoft_heavy_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/cow/step_4.ogg` | `impactSoft_heavy_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/sheep/ambient_1.ogg` | `sheep_baa.flac` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/sheep-baa |
| `entity/sheep/ambient_2.ogg` | `sheep1.flac` | Lamoot | CC BY 3.0 | https://opengameart.org/content/sheep-sound-bleats-yo-frankie |
| `entity/sheep/ambient_3.ogg` | `sheep2.flac` | Lamoot | CC BY 3.0 | https://opengameart.org/content/sheep-sound-bleats-yo-frankie |
| `entity/sheep/ambient_4.ogg` | `sheepBleet.flac` | Lamoot | CC BY 3.0 | https://opengameart.org/content/sheep-sound-bleats-yo-frankie |
| `entity/sheep/step_1.ogg` | `impactSoft_medium_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/sheep/step_2.ogg` | `impactSoft_medium_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/sheep/step_3.ogg` | `impactSoft_medium_002.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/sheep/step_4.ogg` | `impactSoft_medium_003.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/sheep/step_5.ogg` | `impactSoft_medium_004.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/sheep/shear_1.ogg` | `knifeSlice.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/sheep/shear_2.ogg` | `knifeSlice2.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/chicken/ambient_1.ogg` | `fs424076_chicken_sound_-_clucking.mp3 0.08–0.41 с` | chanyatenn | CC0 1.0 | https://freesound.org/people/chanyatenn/sounds/424076/ |
| `entity/chicken/ambient_2.ogg` | `fs424076_chicken_sound_-_clucking.mp3 2.07–2.32 с` | chanyatenn | CC0 1.0 | https://freesound.org/people/chanyatenn/sounds/424076/ |
| `entity/chicken/ambient_3.ogg` | `fs668803_Chicken_clucking_3.mp3 0–0.6 с` | MBPL | CC0 1.0 | https://freesound.org/people/MBPL/sounds/668803/ |
| `entity/chicken/ambient_4.ogg` | `fs607245_Chicken.mp3 0.9–1.58 с` | D4XX | CC0 1.0 | https://freesound.org/people/D4XX/sounds/607245/ |
| `entity/chicken/ambient_5.ogg` | `fs118960_Chickens.mp3 0–0.7 с` | esperri | CC0 1.0 | https://freesound.org/people/esperri/sounds/118960/ |
| `entity/chicken/hurt_1.ogg` | `fs316920_Chicken_Single_Alarm_Call.mp3 0–0.6 с` | Rudmer_Rotteveel | CC0 1.0 | https://freesound.org/people/Rudmer_Rotteveel/sounds/316920/ |
| `entity/chicken/hurt_2.ogg` | `fs316920_Chicken_Single_Alarm_Call.mp3 0.45–1.05 с` | Rudmer_Rotteveel | CC0 1.0 | https://freesound.org/people/Rudmer_Rotteveel/sounds/316920/ |
| `entity/chicken/step_1.ogg` | `impactGeneric_light_000.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/chicken/step_2.ogg` | `impactGeneric_light_001.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/impact-sounds |
| `entity/chicken/egg_1.ogg` | `plop_01.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/100-cc0-sfx |
| `entity/chicken/egg_2.ogg` | `plop_02.ogg` | rubberduck | CC0 1.0 | https://opengameart.org/content/100-cc0-sfx |
| `entity/zombie/ambient_1.ogg` | `undead-1.ogg` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/undead-moans |
| `entity/zombie/ambient_2.ogg` | `undead-2.ogg` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/undead-moans |
| `entity/zombie/ambient_3.ogg` | `undead-3.ogg` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/undead-moans |
| `entity/zombie/ambient_4.ogg` | `undead-4.ogg` | AntumDeluge | CC0 1.0 | https://opengameart.org/content/undead-moans |
| `entity/zombie/ambient_5.ogg` | `ZombieMoans - Track 5 - ZombieMoan1.wav` | EmoPreben | CC0 1.0 | https://opengameart.org/content/zombie-moans-01-by-emopreben |
| `entity/zombie/ambient_6.ogg` | `ZombieMoans - Track 7 - ZombieMoan3.wav` | EmoPreben | CC0 1.0 | https://opengameart.org/content/zombie-moans-01-by-emopreben |
| `entity/zombie/hurt_1.ogg` | `zombieYell1.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/hurt_2.ogg` | `zombieYell2.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/hurt_3.ogg` | `zombieYell4.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/hurt_4.ogg` | `zombieYell6.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/hurt_5.ogg` | `zombieYell9.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/death_1.ogg` | `zombieDeath1.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/death_2.ogg` | `zombieDeath2.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/death_3.ogg` | `zombieDeath3.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/death_4.ogg` | `zombieDeath4.wav` | ArcadeParty | CC0 1.0 | https://opengameart.org/content/zombie-skeleton-monster-voice-effects |
| `entity/zombie/step_1.ogg` | `footstep00.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/zombie/step_2.ogg` | `footstep01.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/zombie/step_3.ogg` | `footstep02.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/zombie/step_4.ogg` | `footstep03.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/zombie/step_5.ogg` | `footstep04.ogg` | Kenney | CC0 1.0 | https://kenney.nl/assets/rpg-audio |
| `entity/generic/extinguish_fire_1.ogg` | `fs155660_cig_extinguish.mp3` | the_semen_incident | CC0 1.0 | https://freesound.org/people/the_semen_incident/sounds/155660/ |
| `entity/generic/extinguish_fire_2.ogg` | `fs234782_Steam_hiss.mp3` | wubitog | CC0 1.0 | https://freesound.org/people/wubitog/sounds/234782/ |
