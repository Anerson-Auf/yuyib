# Runtime, ECS и события

**Статус:** Experimental  
**Requires:** `yuyib-core`, `yuyib-ecs`

`Runtime` задаёт frame boundary. `Runtime::begin_frame` возвращает `FrameInfo`
с zero-based `index`, `delta` и `elapsed`. На первом вызове
`RuntimeEvent::Started` уже виден в этом первом frame: runtime ставит его в
очередь непосредственно перед продвижением buffer. События, опубликованные
в одном уже выполняющемся frame, доступны для чтения в следующем. Это предотвращает изменение
коллекции событий во время обхода и делает delivery semantics предсказуемой.

ECS facade сейчас re-export'ит `bevy_ecs` и его `prelude`. Он изолирует chosen
backend в crate boundary, но пока не добавляет собственный scheduler, World
wrapper или gameplay API. Поэтому production code может импортировать
`yuyib::ecs::prelude::*`, но должен считать этот facade Experimental.

## Commands и Domain Events

Input action выражает намерение пользователя. Interaction system интерпретирует это намерение и создаёт command. После успешной мутации мира публикуется domain event. Quest, UI, audio и network replication подписываются на domain event, но не на hardware key напрямую.

## Limits & Caveats

`FrameEvents` — локальный frame-boundary buffer. Это не persistent event log,
не cross-thread queue и не network replication protocol. `FrameEvents::send`
не делает событие видимым сразу: host обязан вызвать `advance_frame` (у
`Runtime` это происходит внутри `begin_frame`). Эти задачи будут отдельными
opt-in modules.
