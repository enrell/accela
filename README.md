# lain

Terminal acelerado por GPU (GPUI), com núcleo de emulação próprio e sem dependências
de interface. Linux / Wayland e X11.

```
crates/
  lain-vt    emulador: parser VT, tela, protocolos, teclado, mouse, seleção, imagens  (só unicode-width)
  lain-pty   PTY sobre libc, endurecido                                               (só libc)
  lain       app GPUI: abas, divisões, busca, renderização, eventos, IME, clipboard, config
  lain-e2e   harness: programas reais (vim, nvim, htop, less…) rodando em lain-pty + lain-vt
```

## Uso

```sh
scripts/install.sh                 # compila em release e instala em ~/.local/bin + atalho no menu
lain                               # abre o shell ($LAIN_SHELL > `shell` na config > $SHELL)
lain -d ~/projetos -e htop         # diretório inicial e comando
```

| Atalho | Ação |
|---|---|
| Ctrl+Shift+T | nova aba (no diretório atual) |
| Ctrl+Tab / Ctrl+Shift+Tab, Ctrl+Shift+PageDown/PageUp | próxima / anterior aba; clique na aba troca, botão do meio fecha |
| Ctrl+Shift+Alt+PageUp/PageDown | mover a aba para a esquerda/direita (ou arrastar a aba) |
| Ctrl+Shift+D / Ctrl+Shift+E | dividir lado a lado / em cima e embaixo |
| Ctrl+Shift+H/J/K/L | mover o foco para o painel à esquerda/abaixo/acima/à direita |
| Ctrl+Shift+Alt+H/J/K/L | redimensionar o painel (ou arrastar o divisor com o mouse) |
| Ctrl+Shift+W | fechar o painel (sair do shell também fecha) |
| Ctrl+Shift+N | nova janela no diretório atual (OSC 7) |
| Ctrl+Shift+F | buscar no histórico: Enter = anterior, Shift+Enter = próximo, Esc fecha |
| Ctrl+Shift+C / Ctrl+Shift+V | copiar / colar |
| Shift+Insert, botão do meio | colar a seleção primária |
| Shift+PageUp/PageDown/Home/End | rolar o histórico |
| Ctrl+Shift+↑ / ↓ | pular para o prompt anterior / seguinte (OSC 133) |
| Ctrl+= / Ctrl+- / Ctrl+0 | zoom |
| barra de rolagem | aparece à direita quando há histórico; clique ou arraste |
| Ctrl+clique | abrir link (OSC 8 ou URL no texto) |
| clique duplo / triplo / Alt+arrastar | selecionar palavra / linha / bloco |
| Shift + mouse | seleciona mesmo quando o programa captura o mouse |

Todos os atalhos são configuráveis (abaixo). Configuração em `~/.config/lain/config` (ou `$LAIN_CONFIG`). Chaves inválidas geram aviso
e caem no padrão, então o terminal sempre abre:

```ini
font_family = JetBrainsMono Nerd Font Mono
font_size = 15
line_height = 1.25
padding = 6
scrollback = 10000
cursor_blink = true
bold_is_bright = false
gpu = auto              # auto (integrada em híbridos) | discrete | any
background = #1a1b26
foreground = #c0caf5
color1 = #f7768e        # color0 … color255

# Atalhos: bind = <teclas> <ação>  |  <teclas> none (devolve a tecla ao programa)  |  clear
bind = alt+t new_tab
bind = ctrl+shift+w none
```

Ações: `new_tab`, `new_window`, `close_pane`, `split_right`, `split_down`,
`focus_left/down/up/right`, `resize_left/down/up/right`, `next_tab`, `prev_tab`,
`move_tab_left`, `move_tab_right`, `copy`, `paste`, `paste_primary`, `search`, `zoom_in`,
`zoom_out`, `zoom_reset`, `scroll_page_up`, `scroll_page_down`, `scroll_top`, `scroll_bottom`,
`prev_prompt`, `next_prompt`. Modificadores: `ctrl`, `shift`, `alt`, `super`.

A busca diferencia maiúsculas só se o termo tiver alguma ("error" acha "Error"; "Error" não
acha "error"), encontra texto que atravessa quebras de linha automáticas e destaca todas as
ocorrências. Se a busca começa com texto selecionado, ele vira o termo.

`LAIN_DEBUG_KEYS=1 lain` mostra no stderr cada tecla que chega, útil para diagnosticar atalhos
interceptados pelo compositor.

## Compatibilidade

Anuncia `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=lain`, e cumpre essa
entrada inteira: um teste lê o terminfo instalado e verifica **todas** as 104 capacidades de
saída (efeito exato), as 148 de teclado (byte a byte) e os 26 caracteres gráficos (`acsc`).
Exceções documentadas no teste: os dígitos e o Enter do teclado numérico, que chegam do sistema
iguais às teclas principais.

| Área | Suporte |
|---|---|
| Base | VT100/VT220/xterm: movimento, edição (ICH/DCH/IL/DL/ECH/REP), região de scroll, **margens esquerda/direita (DECLRMM/DECSLRM)**, origin mode, tabs, charsets DEC (G0–G3, SO/SI, SS2/SS3), DECALN, RIS/DECSTR |
| Texto | UTF-8, caracteres largos (CJK, emoji), combinantes e ZWJ, reflow ao redimensionar, box-drawing e blocos desenhados por geometria (linhas sempre conectadas) |
| Estilo | SGR completo: 16/256/truecolor (formas `;` e `:`), negrito, dim, itálico, sublinhado simples/duplo/ondulado/pontilhado/tracejado com cor (SGR 58), riscado, overline, inverso, oculto, reverse video |
| Telas | 47 / 1047 / 1048 / 1049, scrollback com limite e compactação |
| Teclado | xterm legado (modificadores, app cursor/keypad, operadores do teclado numérico), **protocolo de teclado do kitty** (todas as flags, pilha por tela), IME/compose |
| Mouse | 9 / 1000 / 1002 / 1003 com codificações X10, UTF-8 (1005), SGR (1006), urxvt (1015), SGR-pixels (1016); rolagem alternativa (1007) |
| Modos modernos | bracketed paste (2004), **synchronized output** (2026, com timeout), foco (1004), tema claro/escuro (996 / 2031) |
| Consultas | DA1/DA2/DA3, XTVERSION, DSR/CPR, DECRQM, DECRQSS, XTGETTCAP, XTWINOPS 14/16/18, XTSMGRAPHICS, cores OSC 4/10/11/12 (responde com o mesmo terminador da pergunta) |
| OSC | título (0/2) + pilha (22/23), cwd (7), hyperlinks (8), notificações (9 / 777), clipboard (52, só escrita), integração com shell (133), resets 104/110/111/112 |
| Imagens | **kitty graphics** (RGB/RGBA/PNG, envio em partes, `a=t/T/p/q/d`, z-index, `C=1`, ancorada no texto ao rolar e ao fazer reflow) e **sixel** (com raster attributes, cores RGB e HLS) |
| Cursor | bloco/barra/sublinhado, piscar (DECSCUSR), contorno vazado sem foco |

Testado de ponta a ponta com: bash/readline, fish, vim, neovim, less, htop, btop, nano,
curses (Python), git + pager, tput.

## Segurança

Nada que um programa escreve na tela pode virar comando ou vazar dados:

- **Sem eco de dados controlados pelo programa**: CSI 21 t (devolver o título) e a leitura
  do clipboard por OSC 52 são recusados, porque são vetores clássicos de injeção e exfiltração.
- **Colar é seguro**: caracteres de controle são removidos, então um texto colado não fecha o
  bracketed paste (`ESC[201~`) nem executa sequências.
- **Imagens só in-band**: a transmissão por arquivo, arquivo temporário ou memória
  compartilhada (`t=f/t/s`) é recusada, então um programa (inclusive remoto) não faz o
  terminal ler arquivos locais.
- **Links**: só abre `http(s)`, `file`, `mailto` e `ftp`, e só com Ctrl+clique.
- **Limites de memória** para tudo que vem do programa: tamanho de tela, título, pilhas,
  hyperlinks, strings DCS/APC, imagens (64 MiB por imagem, 256 MiB no total, removendo as
  mais antigas), sixel (≈4K) e contadores de repetição. Um `cat /dev/urandom` não derruba o terminal.
- **Títulos e notificações** passam por remoção de caracteres de controle e têm tamanho limitado.
  `notify-send` é chamado sem shell.
- **PTY**: todos os fds são `O_CLOEXEC` e o filho fecha tudo acima de 2 antes do `exec`; a máscara
  de sinais e as disposições são limpas; depois do `fork` só há chamadas async-signal-safe (o
  ambiente e o `PATH` são resolvidos antes). Ao fechar: SIGHUP, depois SIGKILL após uma
  carência, e o processo filho é coletado (sem zumbis).

## Performance e consumo

Medido no app real (build release, janela 933×1000, Intel + NVIDIA híbrido):

| Situação | Memória (RSS / anônima) | CPU |
|---|---|---|
| Ocioso | **45 MB / 21 MB** | 0%, 0 despertares/s (cursor sem piscar) |
| Histórico cheio (10k linhas) | 55 MB / 30 MB | — |
| Depois de 21 MB de saída | 63 MB / 38 MB, estável entre execuções | volta a ~0 |

**Vazão.** O núcleo processa 170–460 MB/s:

```sh
cargo run --release -p lain-vt --example bench
```

| Carga (200×50, 10k de scrollback) | Vazão do núcleo |
|---|---|
| ASCII | ~460 MB/s |
| linhas curtas (`seq`) | ~170 MB/s |
| logs coloridos (256 cores + truecolor) | ~220 MB/s |
| Unicode misto (CJK, emoji, combinantes) | ~110 MB/s |
| redesenho de tela cheia (estilo vim/htop) | ~950 MB/s |

O limite de verdade é o PTY do kernel: a camada tty do Linux entrega em média ~38 bytes por
leitura, e mesmo sem terminal nenhum a saída chega a ~30 MB/s
(`cargo run --release -p lain-e2e --example pty_throughput`). O app atinge **~24 MB/s, cerca de
80% desse teto**, com `seq` ou `cat`.

O que mantém o consumo baixo:

- **GPU:** só os drivers Vulkan das GPUs presentes são carregados. Em notebooks híbridos o
  terminal usa a integrada e não acorda a dedicada: isso caiu de 150 para 45 MB de RSS, sem LLVM
  nem a pilha OpenGL da NVIDIA. `gpu = discrete` (ou `any`) na config muda isso, e
  `VK_LOADER_DRIVERS_SELECT` do usuário sempre é respeitado. A variável que o lain define para
  si **não** passa para o shell, então programas abertos nele veem o ambiente original.
- **Linhas:** cada linha guarda até onde foi escrita. Limpar ou recortar uma linha toca só esse
  trecho (7 células para `seq`, não 200). Ao rolar, a linha da tela é reaproveitada e o histórico
  recebe uma cópia do tamanho exato.
- **Leitura do PTY:** em thread própria, drena tudo o que já está disponível num só lote (até
  256 KB). Isso evita acordar a interface centenas de milhares de vezes. O canal é limitado, então
  se a interface atrasar o programa recebe *backpressure*, sem acúmulo de memória. A escrita
  também tem thread própria (colar algo grande não trava a janela).
- **Desenho:** a tela só é redesenhada quando há saída nova. Synchronized output segura o frame
  até o fim da atualização. O timer de piscar só roda com foco. Texto é agrupado por estilo e
  espaços finais não são desenhados.
- **Janela ociosa não acorda:** o GPUI original pedia um *frame callback* ao compositor a
  cada quadro, acordando a thread principal ~60×/s para sempre. Um patch pequeno no GPUI
  (`third_party/gpui/LAIN_PATCH.md`, com o diff) faz os quadros serem pedidos só quando há
  algo a desenhar: parado, **0 despertares/s** (`scripts/idle-wakeups.sh`). Saída gradual,
  digitação, abas, cursor piscando e redimensionamento pelo compositor foram verificados na
  janela real.

## Testes

```sh
scripts/check.sh                          # gate: fmt + clippy (-D warnings) + todos os testes, em segundos
LAIN_FUZZ_CASES=20000 scripts/check.sh    # + fuzz profundo (~6M operações)
scripts/coverage.sh [--html]              # cobertura dos crates do núcleo
```

A pirâmide foi pensada para ser rápida e barata. Nenhum teste usa `sleep`: tudo espera por
uma condição (texto na tela, EOF, modo ativo) com timeout, então a suíte é determinística.

| Camada | Onde | O que garante |
|---|---|---|
| Unitários | `src/*` (`#[cfg(test)]`) | parser, células, cores, base64, teclado, mouse, seleção, URLs, busca, imagens, config, montagem do frame, box-drawing, barra de rolagem, árvore de divisões (layout, vizinhos, redimensionar, arrastar), atalhos, CLI |
| Conformidade | `lain-vt/tests/screen.rs` | comportamento VT/xterm de cada sequência (cursor, edição, scroll, wrap, margens, charsets, telas) |
| Contrato terminfo | `lain-vt/tests/terminfo.rs` | toda capacidade de `xterm-256color` (saída, teclas, acsc, formatos de resposta); capacidade nova não classificada faz o teste falhar |
| Protocolos e segurança | `lain-vt/tests/protocols.rs` | respostas exatas às consultas e recusa do que é perigoso |
| Reflow | `lain-vt/tests/reflow.rs` | redimensionar sem perder texto, cursor, largos, combinantes, marcas |
| Imagens | `lain-vt/tests/graphics.rs` | posicionamento, rolagem, reflow, limpeza e limites |
| Fuzz | `lain-vt/tests/fuzz.rs` | fluxos aleatórios (semente fixa, reproduzível) + todo byte após todo estado do parser; checa invariantes estruturais a cada passo |
| PTY | `lain-pty/tests/pty.rs` | processos reais: saída, status, sinais, tamanho, ambiente, vazamento de fds, máscara de sinais, Ctrl+C, limpeza |
| Interface (headless) | `lain/src/workspace.rs` (`gui_tests`) | o `Workspace` real com terminais e shells reais, dirigido por teclas no despacho normal do GPUI, sem janela nem GPU: abas (abrir, trocar, mover), divisões (criar, foco, redimensionar, fechar), atalhos configurados, e `exit` digitado fechando só aquele painel |
| Ponta a ponta | `lain-e2e/tests/apps.rs` | programas reais renderizam e respondem; pula se o programa não estiver instalado |

Estado atual: **248 testes, ~2 s** (após compilar) e **93% de cobertura de linhas** no núcleo
(`lain-vt` 93%, parser 97%). O fuzz já encontrou e travou 5 bugs reais (caracteres largos
órfãos em casos de borda) antes de chegarem ao uso.
