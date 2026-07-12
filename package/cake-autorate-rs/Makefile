# SPDX-License-Identifier: GPL-2.0-or-later

include $(TOPDIR)/rules.mk

PKG_NAME:=cake-autorate-rs
PKG_VERSION:=1.0_rc1
PKG_RELEASE:=8

PKG_MAINTAINER:=woffko
PKG_LICENSE:=GPL-2.0-or-later

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/cake-autorate-rs
  SECTION:=net
  CATEGORY:=Network
  TITLE:=Rust daemon for adaptive CAKE bandwidth control
  URL:=https://github.com/woffko/cake-autorate-rs-owrt
  MAINTAINER:=woffko
  DEPENDS:=$(RUST_ARCH_DEPENDS) +uci +fping
endef

define Package/cake-autorate-rs/description
  Rust daemon prototype for cake-autorate on OpenWrt. The first backend
  supports UCI configuration, fping/fping-ts/tsping/irtt/ping probing, sysfs byte
  counters, tc-based CAKE shaper updates, optional MQTT publishing, procd
  service integration, and LuCI status.
endef

define Package/cake-autorate-rs/conffiles
/etc/config/cake-autorate
endef

define Package/cake-autorate-rs/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	/etc/init.d/cake-autorate enable >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-mqtt enable >/dev/null 2>&1 || true; \
	exit 0; \
}
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) ./src/* $(PKG_BUILD_DIR)/
endef

define Package/cake-autorate-rs/install
	$(INSTALL_DIR) $(1)/usr/sbin
	$(INSTALL_BIN) $(PKG_INSTALL_DIR)/bin/cake-autorated $(1)/usr/sbin/
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/etc/config/cake-autorate $(1)/etc/config/cake-autorate
	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/etc/init.d/cake-autorate $(1)/etc/init.d/cake-autorate
	$(INSTALL_BIN) ./files/etc/init.d/cake-autorate-mqtt $(1)/etc/init.d/cake-autorate-mqtt
	$(INSTALL_DIR) $(1)/usr/libexec/cake-autorate-rs
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/mqtt-publisher $(1)/usr/libexec/cake-autorate-rs/mqtt-publisher
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/mqtt-status $(1)/usr/libexec/cake-autorate-rs/mqtt-status
endef

$(eval $(call BuildPackage,cake-autorate-rs))
