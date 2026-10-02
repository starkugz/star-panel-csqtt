# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M7] LuCI-приложение CSQTT (современный стек: JS-вью + menu.d + rpcd-ucode).
# Установка дерева LuCI (htdocs/.../view/csqtt → /www/luci-static/... по правилу
# luci.mk HTDOCS=/www, root/usr/share/luci/menu.d, root/usr/share/rpcd/ucode +
# acl.d) и i18n (po/) выполняет luci.mk из luci-feed.
# ПРИНЦИП ИЗОЛЯЦИИ: uci-defaults/firewall-файлов в root/ нет — luci.mk их и не
# ставит (LUCI_DEFAULTS пуст).

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-csqtt
# Версия панели LuCI — 1.0.0 (первый публичный выпуск). Версия встроенного
# ядра (2.1.9) независима и отображается на странице «О программе».
PKG_VERSION:=1.0.0
PKG_RELEASE:=4

PKG_MAINTAINER:=amurcanov
PKG_LICENSE:=PolyForm-Noncommercial-1.0.0

# Персональная редакция панели: star-panel-csqtt 1.0, оформление и UI,
# автор редакции — starkugz. Ядро CSQTT и его версия (2.1.9) не менялись;
# исходный проект (amurcanov), лицензия PolyForm-Noncommercial-1.0.0 и
# обязательные уведомления сохранены (см. раздел «О программе» в LuCI).
LUCI_TITLE:=star-panel-csqtt — LuCI panel for CSQTT
# ucode-mod-socket: rpcd-ucode бекенд импортирует модуль "socket" (loopback API
# Web Helper'а). coreutils-timeout: даёт /usr/bin/timeout, которым обёрнут
# каждый exec бекенда. На ванильном OpenWrt это отдельные пакеты — без них
# rpcd не регистрирует ubus-объект csqtt / падают все exec-методы.
LUCI_DEPENDS:=+csqtt +luci-base +rpcd-mod-ucode +ucode-mod-socket +coreutils-timeout
LUCI_PKGARCH:=all

# [M8] Исходники в рабочем дереве (drvfs/OneDrive) отдаются как 0777, а luci.mk
# ставит их через `cp -pR`, сохраняя права. Из-за world-writable rpcd по
# контракту безопасности отказывается грузить ucode-плагин
# ("Ignoring ucode script ... because it is world writable"), а вью .js попадали
# в пакет с 0777. Нормализуем права build_dir до install.
#
# ВАЖНО: hook должен быть объявлен ДО include luci.mk. BuildPackage раскрывает
# `$(Build/Prepare)` через $(eval) на этапе include, поэтому `Build/Prepare/
# <LUCI_NAME>`, определённый после, уже не попадает в правило. LUCI_NAME здесь
# задаём явно — luci.mk использует `LUCI_NAME?=`, так что значение совпадает.
LUCI_NAME:=luci-app-csqtt
define Build/Prepare/luci-app-csqtt
	$(FIND) $(PKG_BUILD_DIR) -type f -exec chmod 0644 {} + 2>/dev/null || true
	$(FIND) $(PKG_BUILD_DIR) -type d -exec chmod 0755 {} + 2>/dev/null || true
endef

include $(TOPDIR)/feeds/luci/luci.mk

# call BuildPackage - OpenWrt buildroot signature
