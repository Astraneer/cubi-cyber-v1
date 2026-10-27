# Motion namespace 19 для физических досок

Опциональный `RecipeBoardMotionGpu` работает только с sample1 и рисует один batch в уже существующий depth-Equal MotionTargets pass. Он использует тот же invariant vertex pose и текущие 64-байтные записи, что и основной непрозрачный цвет. Дополнительные motion textures, depth prepass, timestamp source, sampler и GPU readback в продукт не добавляются. Default MSAA4 этот владелец не создаёт.

Surface ID хранит namespace19 и номер фактического треугольника панели, полные 64 бита серверной сессии и проверяемое поколение жизни панели. Генерация увеличивается при появлении после скрытия, смене геометрии или сессии. Хеша, float-представления UID и реконструкции ID из позиции нет. Новый/повторно появившийся объект сначала публикует текущий ID с invalid motion, чтобы seed истории не зависел от наличия предыдущего кадра.

Предыдущие позы и unjittered camera берутся исключительно из последнего PRESENTED token. Владелец держит фиксированные CPU front/candidate banks загруженных instance records; один GPU buffer предыдущих записей получает committed front. Очередная загрузка, кодирование и отменённый кадр не продвигают front. `validate_commit` — чистая проверка; общий View сначала проверяет всех участников, затем меняет их front. Неподанный encoder и GPU-completed-but-not-presented candidate отбрасываются через тот же точный FrameToken.

Размер GPU буферов — **96N + 96 байт**: 64N предыдущие позы, 32N пары current/previous identity и 96 байт camera/jitter/flags. Девять досок требуют **960 байт** и один дополнительный pipeline при loading. Эти 960 байт включены в общий точный preflight 256 MiB вместе с остальными владельцами TAA. Новых queries нет; общий предел 15 sources / 45 pairs не расширяется. Фактические upload bytes считаются отдельно от консервативной полной резервации.

Native proof: 576 пикселей проходят настоящий основной цвет и Equal motion; проверены полный session64, поколения при hidden/reuse/session ABA, статические векторы exact0, независимый f64 pan с разными jitter, предыдущая PRESENTED камера после discarded candidate, атомарный отказ upload budget с повтором тем же token и 1000 статичных stage/discard без allocations/uploads. Naga принимает точный production shader с тремя vertex storage buffers.

Dawn proof использует точные Rust-generated основной и motion shaders. Семь случаев: bootstrap, static, два pan с различным jitter, новое поколение, новая сессия и скрытая панель. Во всех видимых случаях 576 независимых свидетелей; статические motion exact0, ошибка f64 pan0, ID точны. Новая жизнь/сессия не читает историю, скрытая панель оставляет ID0. Команды: `cargo run --locked -p render --example recipe_board_motion_inputs -- .local/recipe-board-motion-shaders`, затем `node tools/recipe_board_motion_gpu_smoke.mjs`.

Общая Native/Dawn View-проверка с досками и реальный product mode2 остаются отдельным следующим барьером. Переключатель mode2 до их приёмки скрыт.
