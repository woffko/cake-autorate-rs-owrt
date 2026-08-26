# SPDX-License-Identifier: GPL-2.0-or-later

include $(TOPDIR)/rules.mk
ifeq ($(DISABLE_FAKEROOT),1)
override FAKEROOT :=
endif

PKG_NAME:=cake-autorate-rs
PKG_VERSION:=1.0_rc27
PKG_RELEASE:=311

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
  DEPENDS:=$(RUST_ARCH_DEPENDS) +uci +fping +uclient-fetch +sqm-scripts
  USERID:=cake-speedtest:cake-speedtest
endef

define Package/cake-autorate-rs
$(call Package/cake-autorate-rs/Default)
  TITLE:=Rust daemon for adaptive CAKE bandwidth control
  VARIANT:=full
  PROVIDES:=cake-autorate-rs-full
  CONFLICTS:=cake-autorate-rs-lite
  DEFAULT_VARIANT:=1
  DEPENDS+=+nftables-json +speedtest-go
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
# a no-op.  The Full package owns the calibration init bridge, so this hook can
# first stop and attest the complete old calibration topology with the new
# binary, then recover state, restart the controller and start one native
# coordinator without depending on the LuCI package installation order.
define Package/cake-autorate-rs/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	if [ "$${PKG_UPGRADE:-0}" = 1 ]; then \
		/etc/init.d/cake-autorate-autotune stop >/dev/null 2>&1 || exit 1; \
		/usr/sbin/cake-autorated --native-apply-recover >/dev/null 2>&1 || exit 1; \
		PKG_UPGRADE=0 /etc/init.d/cake-autorate restart >/dev/null 2>&1 || exit 1; \
	fi; \
	/etc/init.d/cake-autorate-autotune enable >/dev/null 2>&1 || exit 1; \
	PKG_UPGRADE=0 /etc/init.d/cake-autorate-autotune start >/dev/null 2>&1 || exit 1; \
}
endef

define Package/cake-autorate-rs/prerm
[ -n "$${IPKG_INSTROOT}" ] || { \
	/etc/init.d/cake-autorate-autotune stop >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-autotune disable >/dev/null 2>&1 || true; \
	rm -f /etc/rc.d/S*cake-autorate-autotune /etc/rc.d/K*cake-autorate-autotune; \
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
	$(SHELL) ./scripts/render-init-variant.sh $(BUILD_VARIANT) ./files/etc/init.d/cake-autorate $(PKG_BUILD_DIR)/cake-autorate.init
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/cake-autorate.init $(1)/etc/init.d/cake-autorate
	$(INSTALL_DIR) $(1)/usr/libexec/cake-autorate-rs
endef

define Package/cake-autorate-rs/install
	$(call Package/cake-autorate-rs/install/Default,$(1))
	$(INSTALL_BIN) ./files/etc/init.d/cake-autorate-autotune $(1)/etc/init.d/cake-autorate-autotune
	$(INSTALL_BIN) ./files/usr/libexec/cake-autorate-rs/runtime-lock $(1)/usr/libexec/cake-autorate-rs/runtime-lock
endef

define Package/cake-autorate-rs-lite/install
	$(call Package/cake-autorate-rs/install/Default,$(1))
endef

$(eval $(call BuildPackage,cake-autorate-rs))
$(eval $(call BuildPackage,cake-autorate-rs-lite))
