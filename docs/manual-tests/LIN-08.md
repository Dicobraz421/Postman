# LIN-08. Кнопки окна по настройке ОС

Статус: ⏳ (стенда Linux нет — проверка отложена)

GNOME, Postman из артефакта CI.

1. `gsettings set org.gnome.desktop.wm.preferences button-layout 'close,minimize,maximize:'`. Ожидается: кнопки окна
   слева в этом порядке, за ними вкладки; разделы над деревом; шестерёнка справа в заголовке.
2. `… button-layout 'appmenu:minimize,maximize,close'` — без перезапуска раскладка как в Windows.
3. `… button-layout ':close'` — справа только «✕».
4. F11 / полный экран — раскладка Windows без кнопок окна.
