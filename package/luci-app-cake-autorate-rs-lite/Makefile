# SPDX-License-Identifier: Apache-2.0

include $(TOPDIR)/rules.mk
ifeq ($(DISABLE_FAKEROOT),1)
override FAKEROOT :=
endif

PKG_NAME:=luci-app-cake-autorate-rs-lite
PKG_VERSION:=1.0_rc27
PKG_RELEASE:=3

PKG_MAINTAINER:=woffko
PKG_LICENSE:=Apache-2.0

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-cake-autorate-rs-lite
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=Minimal manual LuCI support for CAKE Autorate RS
  URL:=https://github.com/woffko/cake-autorate-rs-owrt
  DEPENDS:=+cake-autorate-rs-lite +luci-base +sqm-scripts
  PROVIDES:=luci-app-cake-autorate-rs luci-app-sqm
  PKGARCH:=all
endef

define Package/luci-app-cake-autorate-rs-lite/description
  Minimal CAKE Autorate RS LuCI interface for manual controller, routing,
  directional SQM, latency probes and adaptive-ceiling configuration. Rating,
  speed tests, Full Auto-Tune and scheduled calibration are not included.
endef

define Package/luci-app-cake-autorate-rs-lite/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	rm -f /tmp/luci-indexcache /tmp/luci-indexcache.*; \
	rm -rf /tmp/luci-modulecache /tmp/luci-modulecache.*; \
	/etc/init.d/cake-autorate-autotune stop >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-autotune disable >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-apply-guard stop >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-apply-guard disable >/dev/null 2>&1 || true; \
	rm -f /etc/rc.d/S*cake-autorate-autotune /etc/rc.d/K*cake-autorate-autotune; \
	rm -f /etc/rc.d/S*cake-autorate-apply-guard /etc/rc.d/K*cake-autorate-apply-guard; \
	/etc/init.d/rpcd reload >/dev/null 2>&1 || true; \
	exit 0; \
}
endef

define Build/Compile
endef

define Package/luci-app-cake-autorate-rs-lite/install
	$(INSTALL_DIR) $(1)/www
	$(CP) ./htdocs/* $(1)/www/
	$(INSTALL_DIR) $(1)/
	$(CP) ./root/* $(1)/
endef

$(eval $(call BuildPackage,luci-app-cake-autorate-rs-lite))
