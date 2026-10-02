# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M7] Пакет csqtt для OpenWrt 25.12.x (apk), aarch64_cortex-a53.
# Схема prebuilt-staging: cargo НЕ запускается внутри SDK. Musl-бинарник
# кладёт scripts/build-all.sh в staging/bin/csqtt (см. docs/PROJECT_CONTRACT.md:
# статическая сборка aarch64-unknown-linux-musl).
#
# ПРИНЦИП ИЗОЛЯЦИИ (M3X/PROJECT_CONTRACT): пакет ставит только бинарник +
# init.d + UCI-конфиг + logrotate. Никаких firewall-файлов и uci-defaults,
# никаких ip-full/маршрутизации — routing layer не существует.

include $(TOPDIR)/rules.mk

PKG_NAME:=csqtt
# Версия пакета интеграции для OpenWrt — 1.0.0 (первый публичный выпуск).
# Встроенное ЯДРО остаётся 2.1.9 (см. csqtt-openwrt/Cargo.toml и
# `csqtt version`); версии панели LuCI и ядра разделены (docs/RELEASE.md).
PKG_VERSION:=1.0.0
PKG_RELEASE:=1
PKGARCH:=aarch64_cortex-a53

PKG_MAINTAINER:=amurcanov
PKG_LICENSE:=PolyForm-Noncommercial-1.0.0

include $(INCLUDE_DIR)/package.mk

define Package/csqtt
  SECTION:=net
  CATEGORY:=Network
  TITLE:=CSQTT tunnel client (interface-only)
  DEPENDS:=+kmod-tun
  MENU:=1
endef

define Package/csqtt/description
  OpenWrt integration for the CSQTT tunnel client (package 1.0.0,
  embedded core 2.1.9, focsq-based). Brings up the fixed TUN interface
  csqtt0 and works interface-only: it does NOT become a default gateway,
  does NOT install routes/rules/tables, does NOT touch system DNS/dnsmasq,
  firewall or NAT. Only processes explicitly bound to csqtt0 (e.g. mihomo
  "interface-name: csqtt0") use the tunnel; the CSQTT/VK/TURN control plane
  stays on WAN.
endef

define Package/csqtt/conffiles
/etc/config/csqtt
endef

# prebuilt-staging: сборка ядра выполняется вне SDK (scripts/build-all.sh).
# Здесь только проверка, что артефакт на месте — иначе понятная ошибка.
define Build/Compile
	if [ ! -x "$(CURDIR)/staging/bin/csqtt" ]; then \
		echo "ERROR: staging/bin/csqtt отсутствует или не исполняемый."; \
		echo "Сначала выполните scripts/build-all.sh (musl-сборка ядра)."; \
		exit 1; \
	fi
endef

define Package/csqtt/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(CURDIR)/staging/bin/csqtt $(1)/usr/bin/csqtt

	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) $(CURDIR)/files/etc/init.d/csqtt $(1)/etc/init.d/csqtt

	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_DATA) $(CURDIR)/files/etc/config/csqtt $(1)/etc/config/csqtt

	$(INSTALL_DIR) $(1)/etc/logrotate.d
	$(INSTALL_DATA) $(CURDIR)/files/etc/logrotate.d/csqtt $(1)/etc/logrotate.d/csqtt
endef

# postinst: регистрация сервиса (procd обнаруживает /etc/init.d/csqtt).
# ВАЖНО: OpenWrt default_postinst сам включает init-скрипты пакета (создаёт
# rc.d-симлинки), поэтому автозапуск процесса появляется при установке. Туннель
# при этом НЕ поднимается, пока UCI `main.enabled='0'` и профиль выключен
# (демон при disabled не выбирает профиль). Сетевых мутаций здесь нет.
define Package/csqtt/postinst
#!/bin/sh
if [ -x /etc/init.d/csqtt ]; then
	chmod 0755 /etc/init.d/csqtt
fi
exit 0
endef

$(eval $(call BuildPackage,csqtt))
