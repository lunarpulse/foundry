# 호스트 런북 편차 기록 (2026-11-27)

블록 2의 `image inspect --format` 템플릿(=`{{json .Config.Cmd}}`)이 이 호스트
daemon 29.4.3(containerd image store)에서 실패한다. null Cmd 키가 템플릿 맵에
없어 docker CLI가 엄격 오류를 낸다(`image.stderr` 참조; `if` 분기로도 회피 불가).
기초 이미지 검사 때 `.Config.User` 누락과 같은 CLI 동작 계열이며 환경 제약이지
제품 결함이 아니다.

대응: `docker image inspect` 전체 JSON(`image-full.json`)을 Python으로 투영해
런북과 동일한 필드의 `image.json`을 만들었고 최초 template error는 그대로
`image.stderr`에 남겼다. `image-check.txt`의 단언(id/user/entrypoint/cmd/workdir
전부 예상값 일치)은 런북 원문과 동일하다. 판정 해석은 동결된 w31-oracle.md가
지배한다.
