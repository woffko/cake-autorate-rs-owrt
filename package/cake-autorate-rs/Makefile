# SPDX-License-Identifier: GPL-2.0-or-later

include $(TOPDIR)/rules.mk
ifeq ($(DISABLE_FAKEROOT),1)
override FAKEROOT :=
endif

PKG_NAME:=cake-autorate-rs
PKG_VERSION:=1.0_rc27
PKG_RELEASE:=235

PKG_MAINTAINER:=woffko
PKG_LICENSE:=GPL-2.0-or-later

PKG_BUILD_DEPENDS:=rust/host
PKG_BUILD_PARALLEL:=1
PKG_BUILD_DIR:=$(BUILD_DIR)/$(PKG_NAME)-$(PKG_VERSION)-$(BUILD_VARIANT)

include $(INCLUDE_DIR)/package.mk
include $(TOPDIR)/feeds/packages/lang/rust/rust-package.mk

define Package/cake-autorate-rs/Default
  SECTION:=net
  CATEGORY:=Network
  URL:=https://github.com/woffko/cake-autorate-rs-owrt
  MAINTAINER:=woffko
  DEPENDS:=$(RUST_ARCH_DEPENDS) +uci +fping +uclient-fetch +sqm-scripts +nftables-json
  USERID:=cake-speedtest:cake-speedtest
endef

define Package/cake-autorate-rs
$(call Package/cake-autorate-rs/Default)
  TITLE:=Rust daemon for adaptive CAKE bandwidth control
  VARIANT:=full
  PROVIDES:=cake-autorate-rs-full
  CONFLICTS:=cake-autorate-rs-lite
  DEFAULT_VARIANT:=1
endef

define Package/cake-autorate-rs-lite
$(call Package/cake-autorate-rs/Default)
  TITLE:=Rust daemon for manual CAKE bandwidth control (Lite)
  VARIANT:=lite
  PROVIDES:=cake-autorate-rs
endef

define Package/cake-autorate-rs/description
  Rust daemon prototype for cake-autorate on OpenWrt. The first backend
  supports UCI configuration, fping/fping-ts/tsping/irtt/ping probing, sysfs byte
  counters, tc-based CAKE shaper updates, optional MQTT publishing, procd
  service integration, and LuCI status.
endef

define Package/cake-autorate-rs-lite/description
  Minimal manual-control variant of CAKE Autorate RS. It keeps the normal
  controller, routing, latency probes, directional SQM and adaptive ceiling,
  but excludes Rating, speed-test calibration, Full Auto-Tune and scheduling.
endef

define Package/cake-autorate-rs/conffiles
/etc/config/cake-autorate
endef

Package/cake-autorate-rs-lite/conffiles = $(Package/cake-autorate-rs/conffiles)

# apk does not run the package's pre-deinstall hook on an in-place upgrade,
# and OpenWrt appends this hook after default_postinst.  start_service() sees
# PKG_UPGRADE=1 and deliberately turns default_postinst's premature start into
# a no-op; this hook then performs the one and only stop/start transaction with
# the newly installed files.  The companion Auto-Tune service owns three
# procd instances which must move to the new binary as one lifecycle; signalling
# only the coordinator leaves it cleanly stopped and strands the other two.
define Package/cake-autorate-rs/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	if [ "$${PKG_UPGRADE:-0}" = 1 ]; then \
		PKG_UPGRADE=0 /etc/init.d/cake-autorate restart >/dev/null 2>&1 || exit 1; \
		if [ -x /etc/init.d/cake-autorate-autotune ]; then \
			/etc/init.d/cake-autorate-autotune restart >/dev/null 2>&1 || exit 1; \
		fi; \
	fi; \
}
endef

define Package/cake-autorate-rs-lite/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	if [ "$${PKG_UPGRADE:-0}" = 1 ]; then \
		PKG_UPGRADE=0 /etc/init.d/cake-autorate restart >/dev/null 2>&1 || exit 1; \
	fi; \
}
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	$(CP) ./src/Cargo.toml ./src/Cargo.lock $(PKG_BUILD_DIR)/
	$(CP) ./src/src $(PKG_BUILD_DIR)/
endef

define Build/Compile
	$(call Build/Compile/Cargo,,$(if $(filter lite,$(BUILD_VARIANT)),--no-default-features))
endef

define Package/cake-autorate-rs/install/Default
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
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/sqm-recover $(1)/usr/libexec/cake-autorate-rs/sqm-recover
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/runtime-lock $(1)/usr/libexec/cake-autorate-rs/runtime-lock
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/cpu-profile $(1)/usr/libexec/cake-autorate-rs/cpu-profile
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/traffic-classifier $(1)/usr/libexec/cake-autorate-rs/traffic-classifier
endef

define Package/cake-autorate-rs/install
	$(call Package/cake-autorate-rs/install/Default,$(1))
	$(INSTALL_DIR) $(1)/etc/uci-defaults
	$(INSTALL_BIN) ./files/etc/uci-defaults/99-cake-autorate-rs-scheduler-engine $(1)/etc/uci-defaults/99-cake-autorate-rs-scheduler-engine
endef

define Package/cake-autorate-rs-lite/install
	$(call Package/cake-autorate-rs/install/Default,$(1))
endef

$(eval $(call BuildPackage,cake-autorate-rs))
$(eval $(call BuildPackage,cake-autorate-rs-lite))
